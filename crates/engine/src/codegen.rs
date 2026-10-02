//! The current view as code (DuckDB SQL, Polars or pandas), to carry on in a
//! notebook or script.
//!
//! The code re-reads the source with the view's filters, search, `WHERE` and sort,
//! so it works at any size. SQL is the same DuckDB SQL the engine runs. Polars is
//! native (`scan_*`, typed filter expressions) when every part has an exact Polars
//! equivalent; otherwise the SQL runs through DuckDB and comes back as a Polars
//! frame, so the rows always match what the app shows. pandas always goes through
//! DuckDB: it's exact and doesn't load whole files.
//!
//! Credentials are never written: S3 code names the AWS profile and lets DuckDB or
//! Polars resolve it, as the app does.

use std::cell::Cell;
use std::path::Path;

use crate::dataset::Dataset;
use crate::error::{Error, Result};
use crate::filter::{Filter, FilterOp, ViewSpec};
use crate::s3::is_remote;
use crate::source::{Format, expand_home};
use crate::sql::{ident, literal};
use crate::types::{ColumnInfo, ColumnKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeFlavor {
    /// A DuckDB query.
    Sql,
    /// Python building a Polars `DataFrame` named `df`.
    Polars,
    /// Python building a pandas `DataFrame` named `df`.
    Pandas,
}

impl CodeFlavor {
    pub fn label(self) -> &'static str {
        match self {
            CodeFlavor::Sql => "SQL (DuckDB)",
            CodeFlavor::Polars => "Polars",
            CodeFlavor::Pandas => "pandas",
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct CodeOptions {
    /// Columns to keep, in order. Empty keeps all.
    pub columns: Vec<String>,
    /// For `s3://` sources: the AWS profile and region to name (never credentials).
    pub aws_profile: Option<String>,
    pub aws_region: Option<String>,
    /// Python only: keep the full result lazy (`lf` for Polars, `rel` for DuckDB) and
    /// make `df` its first `preview_rows` rows. For views too big to load at once.
    pub preview_rows: Option<u64>,
}

/// Python packages the generated code imports.
pub fn python_packages(code: &str) -> Vec<&'static str> {
    let mut packages = Vec::new();
    if code.contains("import polars") {
        packages.push("polars");
    }
    if code.contains("import duckdb") {
        packages.push("duckdb");
    }
    if code.contains(".df()") || code.contains("import pandas") {
        packages.extend(["pandas", "numpy"]);
    }
    if code.contains(".pl()") || code.contains("import pyarrow") {
        packages.push("pyarrow");
    }
    if code.contains("AWS_PROFILE") && code.contains("import polars") {
        // Polars resolves AWS profiles (including SSO) through boto3.
        packages.push("boto3");
    }
    packages
}

/// `view_spec` of `dataset` as code. Fails for data that has no source to re-read
/// (query results, comparisons).
pub fn view_code(dataset: &Dataset, spec: &ViewSpec, flavor: CodeFlavor, options: &CodeOptions) -> Result<String> {
    let source = Source::of(dataset)?;
    let columns = &dataset.columns;
    for name in &options.columns {
        if !columns.iter().any(|c| &c.name == name) {
            return Err(Error::other(format!("There’s no column named “{name}”")));
        }
    }
    match flavor {
        CodeFlavor::Sql => {
            if source.format == Format::Arrow {
                return Err(Error::other("DuckDB SQL can’t read Arrow/Feather files directly; copy as Polars or pandas instead"));
            }
            let mut out = String::new();
            for line in source.duckdb_setup(options) {
                out.push_str(&line);
                out.push_str(";\n");
            }
            out.push_str(&sql_query(&source.duckdb_reader(), spec, columns, options)?);
            out.push_str(";\n");
            Ok(out)
        }
        CodeFlavor::Polars => match polars_native(&source, spec, columns, options) {
            Some(code) => Ok(code),
            None => duckdb_python(&source, spec, columns, options, Frame::Polars),
        },
        CodeFlavor::Pandas => duckdb_python(&source, spec, columns, options, Frame::Pandas),
    }
}

// ------------------------------------------------------------------ sources

struct Source {
    format: Format,
    /// A file, glob or URL; folders become a recursive glob of their files' type.
    path: String,
    /// A folder (or several files): readers union columns by name.
    many: bool,
    /// A local folder, given as is (Polars discovers hive partitions from it).
    folder: Option<String>,
    remote: bool,
    s3: bool,
}

impl Source {
    fn of(dataset: &Dataset) -> Result<Self> {
        let spec = dataset
            .source()
            .ok_or_else(|| Error::other("Only data opened from a file, folder or URL can be turned into code"))?;
        let format = dataset
            .format
            .ok_or_else(|| Error::other("Unknown format; open it with an explicit format first"))?;
        let location = expand_home(&spec.location);
        let remote = is_remote(&location);
        let s3 = location.starts_with("s3://") || location.starts_with("s3a://");
        let is_folder = !remote && Path::new(&location).is_dir();
        let table_format = matches!(format, Format::Delta | Format::Iceberg);
        let (path, folder) = if is_folder && !table_format {
            let ext = dataset
                .files
                .first()
                .and_then(|f| Path::new(&f.path).extension())
                .map(|e| e.to_string_lossy().into_owned())
                .unwrap_or_else(|| default_extension(format).to_string());
            let trimmed = location.trim_end_matches(['/', '\\']);
            (format!("{trimmed}/**/*.{ext}"), Some(trimmed.to_string()))
        } else {
            (location.clone(), None)
        };
        let many = is_folder || dataset.files.len() > 1 || path.contains(['*', '?', '[']);
        Ok(Self { format, path, many, folder, remote, s3 })
    }

    /// DuckDB statements to run before reading (S3 credentials, extensions).
    fn duckdb_setup(&self, options: &CodeOptions) -> Vec<String> {
        let mut setup = Vec::new();
        if self.s3 {
            let mut parts = vec!["TYPE s3".to_string(), "PROVIDER credential_chain".to_string()];
            if let Some(profile) = options.aws_profile.as_deref().filter(|p| !p.is_empty()) {
                parts.push(format!("PROFILE {}", literal(profile)));
            }
            if let Some(region) = options.aws_region.as_deref().filter(|r| !r.is_empty()) {
                parts.push(format!("REGION {}", literal(region)));
            }
            setup.push(format!("CREATE OR REPLACE SECRET ({})", parts.join(", ")));
        }
        setup
    }

    /// The DuckDB table function reading the source.
    fn duckdb_reader(&self) -> String {
        let path = literal(&self.path);
        let union = if self.many { ", union_by_name = true" } else { "" };
        match self.format {
            Format::Parquet => format!("read_parquet({path}{union})"),
            Format::Csv => format!("read_csv({path}, auto_detect = true{union})"),
            Format::Json => format!("read_json_auto({path}{union})"),
            Format::Excel => format!("read_xlsx({path})"),
            Format::Delta => format!("delta_scan({path})"),
            Format::Iceberg => format!("iceberg_scan({path}, allow_moved_paths = true)"),
            // Read with pyarrow into `arrow_table` first (see duckdb_python).
            Format::Arrow => "arrow_table".to_string(),
        }
    }
}

fn default_extension(format: Format) -> &'static str {
    match format {
        Format::Parquet => "parquet",
        Format::Csv => "csv",
        Format::Json => "json",
        Format::Excel => "xlsx",
        Format::Arrow => "arrow",
        Format::Delta | Format::Iceberg => "parquet",
    }
}

// ------------------------------------------------------------------ SQL

fn sql_query(reader: &str, spec: &ViewSpec, columns: &[ColumnInfo], options: &CodeOptions) -> Result<String> {
    let select = if options.columns.is_empty() {
        "*".to_string()
    } else {
        options.columns.iter().map(|c| ident(c)).collect::<Vec<_>>().join(", ")
    };
    let mut sql = format!("SELECT {select}\nFROM {reader}");
    if let Some(where_clause) = spec.where_clause(columns, None)? {
        sql.push_str(&format!("\nWHERE {where_clause}"));
    }
    if let Some(order) = spec.order_clause(columns, None)? {
        sql.push_str(&format!("\nORDER BY {order}"));
    }
    Ok(sql)
}

#[derive(Clone, Copy, PartialEq)]
enum Frame {
    Polars,
    Pandas,
}

/// Python running the view's SQL through DuckDB.
fn duckdb_python(source: &Source, spec: &ViewSpec, columns: &[ColumnInfo], options: &CodeOptions, frame: Frame) -> Result<String> {
    let mut out = String::new();
    out.push_str("import duckdb\n");
    if source.format == Format::Arrow {
        out.push_str("import pyarrow.feather\n");
    }
    out.push('\n');
    out.push_str("con = duckdb.connect()\n");
    for line in source.duckdb_setup(options) {
        out.push_str(&format!("con.sql({})\n", python_string(&line)));
    }
    if source.format == Format::Arrow {
        if source.many {
            return Err(Error::other("Several Arrow files can’t be turned into code yet; open one file"));
        }
        out.push_str(&format!("arrow_table = pyarrow.feather.read_table({})\n", python_string(&source.path)));
    }
    let query = sql_query(&source.duckdb_reader(), spec, columns, options)?;
    let convert = if frame == Frame::Polars { "pl" } else { "df" };
    match options.preview_rows {
        Some(rows) => {
            out.push_str(&format!("rel = con.sql({})\n", python_sql(&query)));
            out.push_str(&format!("df = rel.limit({rows}).{convert}()  # the first {rows} rows; `rel` holds them all\n"));
        }
        None => out.push_str(&format!("df = con.sql({}).{convert}()\n", python_sql(&query))),
    }
    Ok(out)
}

// ------------------------------------------------------------------ Polars

/// Native Polars, or `None` when some part has no exact Polars equivalent.
fn polars_native(source: &Source, spec: &ViewSpec, columns: &[ColumnInfo], options: &CodeOptions) -> Option<String> {
    if !spec.where_sql.trim().is_empty() || !spec.search.trim().is_empty() {
        return None;
    }
    let path = python_string(&source.path);
    let scan = match source.format {
        Format::Parquet => match &source.folder {
            // A folder: Polars reads every file and turns key=value folders into columns.
            Some(folder) => format!("pl.scan_parquet({})", python_string(&format!("{folder}/"))),
            None => format!("pl.scan_parquet({path})"),
        },
        Format::Csv => format!("pl.scan_csv({path})"),
        Format::Json if source.path.ends_with(".jsonl") || source.path.ends_with(".ndjson") => {
            format!("pl.scan_ndjson({path})")
        }
        Format::Arrow if !source.many => format!("pl.scan_ipc({path})"),
        // JSON arrays, Excel, Delta and Iceberg read best (and like the app) through DuckDB.
        _ => return None,
    };
    if source.remote && !source.s3 {
        return None;
    }
    let mut steps = vec![scan];
    let uses_datetime = Cell::new(false);
    for filter in &spec.filters {
        let column = columns.iter().find(|c| c.name == filter.column)?;
        let expr = polars_filter(filter, column, &uses_datetime)?;
        steps.push(format!(".filter({expr})"));
    }
    if !spec.sort.is_empty() {
        let names: Vec<String> = spec.sort.iter().map(|k| python_string(&k.column)).collect();
        let descending: Vec<&str> = spec.sort.iter().map(|k| if k.descending { "True" } else { "False" }).collect();
        steps.push(format!(
            ".sort([{}], descending=[{}], nulls_last=True, maintain_order=True)",
            names.join(", "),
            descending.join(", ")
        ));
    }
    if !options.columns.is_empty() {
        let names: Vec<String> = options.columns.iter().map(|c| python_string(c)).collect();
        steps.push(format!(".select([{}])", names.join(", ")));
    }

    let mut out = String::new();
    if source.s3 {
        out.push_str("import os\n");
    }
    if uses_datetime.get() {
        out.push_str("from datetime import date, datetime\n");
    }
    out.push_str("import polars as pl\n\n");
    if source.s3 {
        if let Some(profile) = options.aws_profile.as_deref().filter(|p| !p.is_empty()) {
            out.push_str(&format!("os.environ.setdefault(\"AWS_PROFILE\", {})\n", python_string(profile)));
        }
        if let Some(region) = options.aws_region.as_deref().filter(|r| !r.is_empty()) {
            out.push_str(&format!("os.environ.setdefault(\"AWS_REGION\", {})\n", python_string(region)));
        }
    }
    let chain = steps.join("\n    ");
    match options.preview_rows {
        Some(rows) => {
            out.push_str(&format!("lf = (\n    {chain}\n)\n"));
            out.push_str(&format!("df = lf.head({rows}).collect()  # the first {rows} rows; `lf` holds them all\n"));
        }
        None => out.push_str(&format!("df = (\n    {chain}\n    .collect()\n)\n")),
    }
    Some(out)
}

/// One filter as a Polars expression with the engine's semantics, or `None`.
fn polars_filter(filter: &Filter, column: &ColumnInfo, uses_datetime: &Cell<bool>) -> Option<String> {
    let col = format!("pl.col({})", python_string(&column.name));
    let value = |text: &str| polars_literal(text, column, uses_datetime);
    let values = |text: &str| -> Option<String> {
        let items: Option<Vec<String>> =
            text.split(',').map(str::trim).filter(|s| !s.is_empty()).map(&value).collect();
        let items = items?;
        (!items.is_empty()).then(|| format!("[{}]", items.join(", ")))
    };
    let text = format!("{col}.cast(pl.String).str.to_lowercase()");
    let lower = |s: &str| python_string(&s.to_lowercase());
    let textual = !column.kind.is_nested() && !matches!(column.kind, ColumnKind::Binary | ColumnKind::Other);
    Some(match filter.op {
        FilterOp::Equals => format!("{col} == {}", value(&filter.value)?),
        // The engine's `IS DISTINCT FROM` keeps nulls.
        FilterOp::NotEquals => format!("{col}.ne_missing({})", value(&filter.value)?),
        FilterOp::Less => format!("{col} < {}", value(&filter.value)?),
        FilterOp::LessOrEqual => format!("{col} <= {}", value(&filter.value)?),
        FilterOp::Greater => format!("{col} > {}", value(&filter.value)?),
        FilterOp::GreaterOrEqual => format!("{col} >= {}", value(&filter.value)?),
        FilterOp::Between => format!("{col}.is_between({}, {})", value(&filter.value)?, value(&filter.value2)?),
        FilterOp::In => format!("{col}.is_in({})", values(&filter.value)?),
        FilterOp::NotIn => format!("~{col}.is_in({}) | {col}.is_null()", values(&filter.value)?),
        FilterOp::Contains if textual => format!("{text}.str.contains({}, literal=True)", lower(&filter.value)),
        FilterOp::NotContains if textual => {
            format!("~{text}.str.contains({}, literal=True) | {col}.is_null()", lower(&filter.value))
        }
        FilterOp::StartsWith if textual => format!("{text}.str.starts_with({})", lower(&filter.value)),
        FilterOp::EndsWith if textual => format!("{text}.str.ends_with({})", lower(&filter.value)),
        FilterOp::IsNull => format!("{col}.is_null()"),
        FilterOp::IsNotNull => format!("{col}.is_not_null()"),
        FilterOp::IsTrue if column.kind == ColumnKind::Boolean => format!("{col} == True"),
        FilterOp::IsFalse if column.kind == ColumnKind::Boolean => format!("{col} == False"),
        FilterOp::IsEmpty if column.kind == ColumnKind::String => format!("{col} == \"\""),
        FilterOp::IsNotEmpty if column.kind == ColumnKind::String => format!("{col} != \"\""),
        FilterOp::IsEmpty if column.kind == ColumnKind::List => format!("{col}.list.len() == 0"),
        FilterOp::IsNotEmpty if column.kind == ColumnKind::List => format!("{col}.list.len() > 0"),
        // Regular expression dialects differ; nested and other kinds go through DuckDB.
        _ => return None,
    })
}

/// A typed Python literal for a filter value, or `None` when it can't be written exactly.
fn polars_literal(text: &str, column: &ColumnInfo, uses_datetime: &Cell<bool>) -> Option<String> {
    let t = text.trim();
    match column.kind {
        ColumnKind::Integer => t.parse::<i128>().ok().map(|v| v.to_string()),
        ColumnKind::Float => {
            let v = t.parse::<f64>().ok().filter(|v| v.is_finite())?;
            Some(format!("{v:?}"))
        }
        ColumnKind::Boolean => match t.to_ascii_lowercase().as_str() {
            "true" | "t" | "1" | "yes" => Some("True".into()),
            "false" | "f" | "0" | "no" => Some("False".into()),
            _ => None,
        },
        ColumnKind::String => Some(python_string(text)),
        ColumnKind::Date => {
            let (y, m, d) = parse_date(t)?;
            uses_datetime.set(true);
            Some(format!("date({y}, {m}, {d})"))
        }
        // Naive timestamps only: comparing a time-zone-aware column needs care.
        ColumnKind::Timestamp if !column.sql_type.to_ascii_uppercase().contains("TIME ZONE") => {
            let (date, time) = t.split_once([' ', 'T']).unwrap_or((t, "00:00:00"));
            parse_date(date)?;
            let valid = time.split(':').count() >= 2 && time.chars().all(|c| c.is_ascii_digit() || c == ':' || c == '.');
            if !valid {
                return None;
            }
            uses_datetime.set(true);
            Some(format!("datetime.fromisoformat({})", python_string(&format!("{date} {time}"))))
        }
        // Decimals, times, intervals, nested: through DuckDB.
        _ => None,
    }
}

fn parse_date(text: &str) -> Option<(i32, u32, u32)> {
    let mut parts = text.split('-');
    let y = parts.next()?.parse().ok()?;
    let m = parts.next()?.parse().ok().filter(|m| (1..=12).contains(m))?;
    let d = parts.next()?.parse().ok().filter(|d| (1..=31).contains(d))?;
    parts.next().is_none().then_some((y, m, d))
}

// ------------------------------------------------------------------ Python text

/// A Python string literal (JSON escaping is valid Python).
fn python_string(text: &str) -> String {
    serde_json::to_string(text).unwrap_or_else(|_| "\"\"".into())
}

/// SQL as a readable multi-line Python string where possible.
fn python_sql(sql: &str) -> String {
    if !sql.contains("\"\"\"") && !sql.ends_with('\\') && !sql.ends_with('"') {
        format!("r\"\"\"\n{sql}\n\"\"\"")
    } else {
        python_string(sql)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn column(name: &str, kind: ColumnKind, sql_type: &str) -> ColumnInfo {
        let column = ColumnInfo::new(name, sql_type);
        assert_eq!(column.kind, kind, "{sql_type}");
        column
    }

    #[test]
    fn literals_and_filters() {
        let dt = Cell::new(false);
        let int = column("n", ColumnKind::Integer, "BIGINT");
        let text = column("s", ColumnKind::String, "VARCHAR");
        let day = column("d", ColumnKind::Date, "DATE");
        let tz = column("t", ColumnKind::Timestamp, "TIMESTAMP WITH TIME ZONE");
        assert_eq!(polars_literal("42", &int, &dt).as_deref(), Some("42"));
        assert_eq!(polars_literal("4.2", &int, &dt), None);
        assert_eq!(polars_literal("say \"hi\"\n", &text, &dt).as_deref(), Some(r#""say \"hi\"\n""#));
        assert_eq!(polars_literal("2024-02-29", &day, &dt).as_deref(), Some("date(2024, 2, 29)"));
        assert!(dt.get());
        assert_eq!(polars_literal("2024-02-29 10:00:00", &tz, &dt), None);
        assert_eq!(
            polars_filter(&Filter::new("n", FilterOp::NotIn, "1, 2"), &int, &dt).as_deref(),
            Some(r#"~pl.col("n").is_in([1, 2]) | pl.col("n").is_null()"#)
        );
        assert_eq!(polars_filter(&Filter::new("s", FilterOp::Matches, "a.*"), &text, &dt), None);
        assert_eq!(python_sql("SELECT 1"), "r\"\"\"\nSELECT 1\n\"\"\"");
        assert_eq!(python_sql("SELECT '\"\"\"'"), "\"SELECT '\\\"\\\"\\\"'\"");
    }

    #[test]
    fn packages_follow_imports() {
        assert_eq!(python_packages("import polars as pl\n"), ["polars"]);
        assert_eq!(python_packages("import duckdb\ndf = con.sql(q).df()"), ["duckdb", "pandas", "numpy"]);
        assert_eq!(python_packages("import duckdb\ndf = con.sql(q).pl()"), ["duckdb", "pyarrow"]);
        assert_eq!(python_packages("import os\nimport polars as pl\nos.environ.setdefault(\"AWS_PROFILE\", \"x\")"), ["polars", "boto3"]);
    }
}
