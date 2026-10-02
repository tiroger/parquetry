//! What the person asked to open, and how to find the files behind it.

use std::path::{Path, PathBuf};

use duckdb::Connection;
use serde::{Deserialize, Serialize};

use crate::Engine;
use crate::error::{Error, Result};
use crate::s3::{S3Url, is_remote};
use crate::sql::literal;

/// File formats Parquetry can open.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Format {
    Parquet,
    Csv,
    Json,
    Arrow,
    Excel,
    Delta,
    Iceberg,
}

impl Format {
    pub fn label(self) -> &'static str {
        match self {
            Format::Parquet => "Parquet",
            Format::Csv => "CSV",
            Format::Json => "JSON",
            Format::Arrow => "Arrow IPC",
            Format::Excel => "Excel",
            Format::Delta => "Delta Lake",
            Format::Iceberg => "Iceberg",
        }
    }

    pub fn all() -> &'static [Format] {
        &[
            Format::Parquet,
            Format::Csv,
            Format::Json,
            Format::Arrow,
            Format::Excel,
            Format::Delta,
            Format::Iceberg,
        ]
    }
}

/// A request to open something: a path, folder, glob or URL, with an optional
/// explicit format (otherwise detected).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SourceSpec {
    pub location: String,
    pub format: Option<Format>,
}

impl SourceSpec {
    pub fn new(location: impl Into<String>) -> Self {
        Self {
            location: location.into(),
            format: None,
        }
    }

    pub fn with_format(mut self, format: Format) -> Self {
        self.format = Some(format);
        self
    }

    pub fn is_remote(&self) -> bool {
        is_remote(&self.location)
    }

    /// Short name for tabs and titles: the file or folder name.
    pub fn display_name(&self) -> String {
        let trimmed = self.location.trim_end_matches(['/', '\\']);
        let name = last_segment(trimmed);
        if name.is_empty() {
            self.location.clone()
        } else {
            name.to_string()
        }
    }
}

/// The part after the last `/` (or `\`, for Windows paths).
fn last_segment(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// Map a file name to a format by extension (ignoring compression suffixes).
pub fn format_from_extension(path: &str) -> Option<Format> {
    let lower = path.to_ascii_lowercase();
    let lower = lower
        .trim_end_matches(".gz")
        .trim_end_matches(".zst")
        .trim_end_matches(".bz2")
        .trim_end_matches(".xz");
    let ext = lower.rsplit_once('.').map(|(_, e)| e)?;
    match ext {
        "parquet" | "parq" | "pq" => Some(Format::Parquet),
        "csv" | "tsv" | "txt" | "psv" | "tab" => Some(Format::Csv),
        "json" | "jsonl" | "ndjson" | "geojson" => Some(Format::Json),
        "arrow" | "feather" | "ipc" | "arrows" => Some(Format::Arrow),
        "xlsx" => Some(Format::Excel),
        _ => None,
    }
}

fn has_glob(location: &str) -> bool {
    location.contains('*') || location.contains('?') || location.contains('[')
}

fn is_hidden_name(name: &str) -> bool {
    name.starts_with('.') || name.starts_with('_')
}

/// The concrete input a dataset reads.
#[derive(Debug, Clone)]
pub(crate) enum Resolved {
    /// Data files of one format.
    Files { format: Format, files: Vec<String> },
    /// A table format read through a DuckDB extension.
    Table { format: Format, root: String },
}

/// Work out the format and file list. Runs on an engine worker (may hit the network).
pub(crate) fn resolve(engine: &Engine, conn: &Connection, spec: &SourceSpec) -> Result<Resolved> {
    let location = spec.location.trim().to_string();
    if location.is_empty() {
        return Err(Error::other("Nothing to open"));
    }
    if is_remote(&location) {
        engine.inner.s3.prepare_duckdb(engine, conn, &location)?;
        return resolve_remote(engine, conn, spec, &location);
    }

    let location = expand_home(&location);
    let path = Path::new(&location);
    if !path.exists() {
        if has_glob(&location) {
            let files = duckdb_glob(conn, &location)?;
            let format = spec
                .format
                .or_else(|| format_from_extension(&location))
                .or_else(|| files.first().and_then(|f| format_from_extension(f)))
                .unwrap_or(Format::Parquet);
            return Ok(Resolved::Files { format, files });
        }
        return Err(Error::other(format!("“{location}” doesn’t exist")));
    }

    if path.is_dir() {
        if spec.format == Some(Format::Delta) || path.join("_delta_log").is_dir() {
            return Ok(Resolved::Table {
                format: Format::Delta,
                root: location,
            });
        }
        if spec.format == Some(Format::Iceberg) || is_local_iceberg(path) {
            return Ok(Resolved::Table {
                format: Format::Iceberg,
                root: location,
            });
        }
        let mut files = Vec::new();
        collect_files(path, &mut files, 200_000)?;
        return files_of_best_format(spec.format, files.iter().map(|p| p.to_string_lossy().into_owned()).collect(), &location);
    }

    let format = match spec.format {
        Some(format) => format,
        None => match format_from_extension(&location) {
            Some(format) => format,
            None => sniff_local(path)?,
        },
    };
    Ok(Resolved::Files {
        format,
        files: vec![location],
    })
}

fn resolve_remote(
    engine: &Engine,
    conn: &Connection,
    spec: &SourceSpec,
    location: &str,
) -> Result<Resolved> {
    if has_glob(location) {
        let files = duckdb_glob(conn, location)?;
        let format = spec
            .format
            .or_else(|| format_from_extension(location))
            .unwrap_or(Format::Parquet);
        return Ok(Resolved::Files { format, files });
    }
    let looks_like_file = format_from_extension(location).is_some() && !location.ends_with('/');
    if looks_like_file || !location.starts_with("s3") {
        let format = spec
            .format
            .or_else(|| format_from_extension(location))
            .unwrap_or(Format::Parquet);
        if matches!(format, Format::Delta | Format::Iceberg) {
            return Ok(Resolved::Table {
                format,
                root: location.to_string(),
            });
        }
        return Ok(Resolved::Files {
            format,
            files: vec![location.to_string()],
        });
    }
    // An S3 prefix: a table folder or a folder of data files.
    let url = S3Url::parse(location).ok_or_else(|| Error::other("Invalid S3 URL"))?;
    let prefix_url = if url.key.is_empty() || url.key.ends_with('/') {
        location.to_string()
    } else {
        format!("{location}/")
    };
    if matches!(spec.format, Some(Format::Delta) | Some(Format::Iceberg)) {
        return Ok(Resolved::Table {
            format: spec.format.unwrap(),
            root: prefix_url.trim_end_matches('/').to_string(),
        });
    }
    let entries = engine.inner.s3.list_recursive(&prefix_url, 200_000)?;
    if entries.is_empty() {
        // Maybe it's an object without an extension.
        return Ok(Resolved::Files {
            format: spec.format.unwrap_or(Format::Parquet),
            files: vec![location.to_string()],
        });
    }
    let prefix_key = S3Url::parse(&prefix_url).map(|u| u.key).unwrap_or_default();
    let relative = |url: &str| -> String {
        S3Url::parse(url)
            .map(|u| u.key.strip_prefix(&prefix_key).unwrap_or(&u.key).to_string())
            .unwrap_or_default()
    };
    if entries.iter().any(|e| relative(&e.url).starts_with("_delta_log/")) {
        return Ok(Resolved::Table {
            format: Format::Delta,
            root: prefix_url.trim_end_matches('/').to_string(),
        });
    }
    if entries.iter().any(|e| {
        let rel = relative(&e.url);
        rel.starts_with("metadata/") && rel.ends_with(".metadata.json")
    }) {
        return Ok(Resolved::Table {
            format: Format::Iceberg,
            root: prefix_url.trim_end_matches('/').to_string(),
        });
    }
    let files: Vec<String> = entries
        .into_iter()
        .filter(|e| {
            relative(&e.url)
                .split('/')
                .all(|segment| !is_hidden_name(segment))
        })
        .map(|e| e.url)
        .collect();
    files_of_best_format(spec.format, files, location)
}

fn files_of_best_format(
    requested: Option<Format>,
    files: Vec<String>,
    location: &str,
) -> Result<Resolved> {
    let preference = [
        Format::Parquet,
        Format::Csv,
        Format::Json,
        Format::Arrow,
        Format::Excel,
    ];
    let candidates: Vec<Format> = match requested {
        Some(format) => vec![format],
        None => preference.to_vec(),
    };
    for format in candidates {
        let mut matching: Vec<String> = files
            .iter()
            .filter(|f| format_from_extension(f) == Some(format))
            .cloned()
            .collect();
        if !matching.is_empty() {
            matching.sort();
            return Ok(Resolved::Files {
                format,
                files: matching,
            });
        }
    }
    Err(Error::other(format!(
        "No Parquet, CSV, JSON or Arrow files found in “{location}”"
    )))
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>, limit: usize) -> Result<()> {
    let mut entries: Vec<_> = std::fs::read_dir(dir)?.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        if out.len() >= limit {
            break;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if is_hidden_name(&name) {
            continue;
        }
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() || (file_type.is_symlink() && path.is_dir()) {
            collect_files(&path, out, limit)?;
        } else if format_from_extension(&name).is_some() {
            out.push(path);
        }
    }
    Ok(())
}

fn is_local_iceberg(path: &Path) -> bool {
    let metadata = path.join("metadata");
    metadata.is_dir()
        && std::fs::read_dir(&metadata)
            .map(|entries| {
                entries.filter_map(|e| e.ok()).any(|e| {
                    e.file_name()
                        .to_string_lossy()
                        .ends_with(".metadata.json")
                })
            })
            .unwrap_or(false)
}

/// Guess a local file's format from its first bytes.
fn sniff_local(path: &Path) -> Result<Format> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut head = [0u8; 8];
    let n = file.read(&mut head)?;
    let head = &head[..n];
    if head.starts_with(b"PAR1") {
        return Ok(Format::Parquet);
    }
    if head.starts_with(b"ARROW1") || head.starts_with(&[0xff, 0xff, 0xff, 0xff]) {
        return Ok(Format::Arrow);
    }
    if head.starts_with(b"PK") {
        return Ok(Format::Excel);
    }
    let first = head.iter().find(|b| !b.is_ascii_whitespace()).copied();
    if matches!(first, Some(b'{') | Some(b'[')) {
        return Ok(Format::Json);
    }
    Ok(Format::Csv)
}

fn duckdb_glob(conn: &Connection, pattern: &str) -> Result<Vec<String>> {
    let mut stmt = conn.prepare(&format!("SELECT file FROM glob({}) ORDER BY file", literal(pattern)))?;
    let files: Vec<String> = stmt
        .query_map([], |row| row.get::<_, String>(0))?
        .filter_map(|r| r.ok())
        .filter(|f| {
            !is_hidden_name(last_segment(f))
        })
        .collect();
    if files.is_empty() {
        return Err(Error::other(format!("No files match “{pattern}”")));
    }
    Ok(files)
}

pub fn expand_home(location: &str) -> String {
    if let Some(rest) = location.strip_prefix("~/")
        && let Some(home) = dirs::home_dir() {
            return home.join(rest).to_string_lossy().into_owned();
        }
    if let Some(rest) = location.strip_prefix("file://") {
        return percent_decode(rest);
    }
    location.to_string()
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len()
            && let Ok(value) =
                u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""), 16)
            {
                out.push(value);
                i += 3;
                continue;
            }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions() {
        assert_eq!(format_from_extension("a.parquet"), Some(Format::Parquet));
        assert_eq!(format_from_extension("a.PQ"), Some(Format::Parquet));
        assert_eq!(format_from_extension("a.csv.gz"), Some(Format::Csv));
        assert_eq!(format_from_extension("a.jsonl"), Some(Format::Json));
        assert_eq!(format_from_extension("a.feather"), Some(Format::Arrow));
        assert_eq!(format_from_extension("a"), None);
        assert_eq!(format_from_extension("dir.v2/file"), None);
    }

    #[test]
    fn names() {
        assert_eq!(SourceSpec::new("/a/b/c.parquet").display_name(), "c.parquet");
        assert_eq!(SourceSpec::new("s3://bucket/dir/").display_name(), "dir");
        assert_eq!(SourceSpec::new("s3://bucket").display_name(), "bucket");
        assert_eq!(SourceSpec::new(r"C:\data\sales.parquet").display_name(), "sales.parquet");
        assert_eq!(SourceSpec::new(r"D:\exports\2024\").display_name(), "2024");
    }

    #[test]
    fn file_urls() {
        assert_eq!(expand_home("file:///tmp/a%20b.parquet"), "/tmp/a b.parquet");
    }
}
