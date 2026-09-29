//! Quick Look preview generator for Parquet files.
//!
//! Produces a self-contained HTML document (inline CSS, no JS, no network) describing a
//! Parquet file: a summary line, the Arrow schema and the first rows. Only the footer and
//! the pages needed for the first `max_rows` rows are read, so previews of multi-GB files
//! stay fast.
//!
//! The crate is built as a static library and linked into the macOS Quick Look extension
//! (`ParquetryQuickLook.appex`), which calls [`parquetry_ql_preview`] /
//! [`parquetry_ql_free`] through the C header in `include/parquetry_ql.h`.

use std::collections::BTreeSet;
use std::ffi::{CStr, c_char};
use std::fmt::{self, Write as _};
use std::fs::File;
use std::path::Path;

use arrow::array::{Array, RecordBatch};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::util::display::{ArrayFormatter, FormatOptions};
use parquet::arrow::ProjectionMask;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::basic::Compression;
use parquet::file::metadata::ParquetMetaData;

/// Maximum number of (top-level) columns rendered in the schema and data tables.
pub const MAX_COLUMNS: usize = 100;
/// Maximum number of characters rendered per data cell.
pub const MAX_CELL_CHARS: usize = 120;
/// Maximum number of characters rendered for a data type in the schema table.
pub const MAX_TYPE_CHARS: usize = 60;

/// Render an HTML preview of the Parquet file at `path`, including at most `max_rows` rows.
pub fn preview_html(path: &Path, max_rows: usize) -> Result<String, String> {
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());

    let file = File::open(path).map_err(|e| format!("Could not open “{file_name}”: {e}"))?;
    let file_size = file.metadata().map(|m| m.len()).ok();

    let builder = ParquetRecordBatchReaderBuilder::try_new(file)
        .map_err(|e| format!("“{file_name}” is not a readable Parquet file: {e}"))?;

    let metadata = builder.metadata().clone();
    let schema = builder.schema().clone();
    let file_meta = metadata.file_metadata();
    let total_rows = u64::try_from(file_meta.num_rows()).unwrap_or(0);
    let shown_cols = schema.fields().len().min(MAX_COLUMNS);

    // Only read the leading row groups that are needed to cover `max_rows`, and only the
    // columns that are actually displayed.
    let batches = if max_rows == 0 || total_rows == 0 || shown_cols == 0 {
        Vec::new()
    } else {
        let mut row_groups = Vec::new();
        let mut covered = 0u64;
        for (i, rg) in metadata.row_groups().iter().enumerate() {
            if covered >= max_rows as u64 {
                break;
            }
            row_groups.push(i);
            covered += u64::try_from(rg.num_rows()).unwrap_or(0);
        }
        let mask = ProjectionMask::roots(builder.parquet_schema(), 0..shown_cols);
        let reader = builder
            .with_row_groups(row_groups)
            .with_projection(mask)
            .with_limit(max_rows)
            .with_batch_size(max_rows)
            .build()
            .map_err(|e| format!("Could not read rows of “{file_name}”: {e}"))?;
        let mut batches = Vec::new();
        for batch in reader {
            batches.push(batch.map_err(|e| format!("Could not decode rows of “{file_name}”: {e}"))?);
        }
        batches
    };

    Ok(render_document(
        &file_name, file_size, &metadata, &schema, total_rows, &batches,
    ))
}

/// A small, styled HTML page describing an error.
pub fn error_html(message: &str) -> String {
    let mut out = String::with_capacity(2048 + message.len());
    out.push_str("<!DOCTYPE html>\n<html><head><meta charset=\"utf-8\">\n");
    out.push_str("<meta name=\"color-scheme\" content=\"light dark\">\n<title>Parquet preview</title>\n<style>");
    out.push_str(BASE_CSS);
    out.push_str(ERROR_CSS);
    out.push_str("</style></head><body>\n<div class=\"error\"><div class=\"error-title\">Can’t preview this file</div>\n<div class=\"error-msg\">");
    push_escaped(&mut out, message);
    out.push_str("</div></div>\n</body></html>\n");
    out
}

// ---------------------------------------------------------------------------------------------
// C ABI
// ---------------------------------------------------------------------------------------------

/// Render a preview of the Parquet file at the NUL-terminated UTF-8 `path`.
///
/// Always returns a UTF-8 HTML buffer (the preview, or an error page), whose length is written
/// to `*out_len`. The buffer is NOT NUL-terminated and must be released with
/// [`parquetry_ql_free`] using the same length. Returns null only if allocation fails.
///
/// # Safety
/// `path` must be null or a valid NUL-terminated string; `out_len` must be null or writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn parquetry_ql_preview(
    path: *const c_char,
    max_rows: u32,
    out_len: *mut usize,
) -> *mut u8 {
    let html = std::panic::catch_unwind(|| {
        if path.is_null() {
            return error_html("No file path was provided.");
        }
        // SAFETY: the caller guarantees `path` is a valid NUL-terminated string.
        let c_path = unsafe { CStr::from_ptr(path) };
        let path = bytes_to_path(c_path.to_bytes());
        match preview_html(&path, max_rows as usize) {
            Ok(html) => html,
            Err(message) => error_html(&message),
        }
    })
    .unwrap_or_else(|_| error_html("An internal error occurred while rendering the preview."));

    let boxed: Box<[u8]> = html.into_bytes().into_boxed_slice();
    let len = boxed.len();
    if !out_len.is_null() {
        // SAFETY: the caller guarantees `out_len` is writable.
        unsafe { *out_len = len };
    }
    Box::into_raw(boxed) as *mut u8
}

/// Free a buffer returned by [`parquetry_ql_preview`]. `len` must be the length reported
/// through `out_len`. Passing null is a no-op.
///
/// # Safety
/// `ptr`/`len` must come from a single previous call to [`parquetry_ql_preview`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn parquetry_ql_free(ptr: *mut u8, len: usize) {
    if ptr.is_null() {
        return;
    }
    // SAFETY: `ptr`/`len` describe a `Box<[u8]>` leaked by `parquetry_ql_preview`.
    unsafe {
        drop(Box::from_raw(std::ptr::slice_from_raw_parts_mut(ptr, len)));
    }
}

#[cfg(unix)]
fn bytes_to_path(bytes: &[u8]) -> std::path::PathBuf {
    use std::os::unix::ffi::OsStrExt;
    std::path::PathBuf::from(std::ffi::OsStr::from_bytes(bytes))
}

#[cfg(not(unix))]
fn bytes_to_path(bytes: &[u8]) -> std::path::PathBuf {
    std::path::PathBuf::from(String::from_utf8_lossy(bytes).into_owned())
}

// ---------------------------------------------------------------------------------------------
// Rendering
// ---------------------------------------------------------------------------------------------

fn render_document(
    file_name: &str,
    file_size: Option<u64>,
    metadata: &ParquetMetaData,
    schema: &Schema,
    total_rows: u64,
    batches: &[RecordBatch],
) -> String {
    let fields = schema.fields();
    let shown_cols = fields.len().min(MAX_COLUMNS);
    let hidden_cols = fields.len() - shown_cols;
    let shown_rows: usize = batches.iter().map(|b| b.num_rows()).sum();

    let mut out = String::with_capacity(16 * 1024 + shown_rows * shown_cols * 24);
    out.push_str("<!DOCTYPE html>\n<html><head><meta charset=\"utf-8\">\n");
    out.push_str("<meta name=\"color-scheme\" content=\"light dark\">\n<title>");
    push_escaped(&mut out, file_name);
    out.push_str("</title>\n<style>");
    out.push_str(BASE_CSS);
    out.push_str(PREVIEW_CSS);
    out.push_str("</style></head><body>\n");

    // Header -----------------------------------------------------------------------------
    out.push_str("<header><h1>");
    push_escaped(&mut out, file_name);
    out.push_str("</h1>\n<div class=\"summary\">");
    let mut parts: Vec<String> = vec![
        plural(total_rows, "row", "rows"),
        plural(fields.len() as u64, "column", "columns"),
        plural(metadata.num_row_groups() as u64, "row group", "row groups"),
    ];
    if let Some(size) = file_size {
        parts.push(format_bytes(size));
    }
    let codecs = compression_codecs(metadata);
    if !codecs.is_empty() {
        parts.push(codecs.join(", "));
    }
    for (i, part) in parts.iter().enumerate() {
        if i > 0 {
            out.push_str("<span class=\"sep\">·</span>");
        }
        out.push_str("<span>");
        push_escaped(&mut out, part);
        out.push_str("</span>");
    }
    out.push_str("</div>\n");
    if let Some(created_by) = metadata.file_metadata().created_by() {
        out.push_str("<div class=\"created\">Created by ");
        push_escaped(&mut out, created_by);
        out.push_str("</div>\n");
    }
    out.push_str("</header>\n");

    // Schema -----------------------------------------------------------------------------
    out.push_str("<section><h2>Schema</h2>\n<div class=\"scroll schema-scroll\"><table class=\"schema\">\n");
    out.push_str("<thead><tr><th class=\"num\">#</th><th>Column</th><th>Type</th><th>Nullable</th></tr></thead>\n<tbody>\n");
    for (i, field) in fields.iter().take(shown_cols).enumerate() {
        let _ = write!(out, "<tr><td class=\"num idx\">{i}</td><td class=\"name\">");
        push_escaped(&mut out, field.name());
        out.push_str("</td><td class=\"type\">");
        push_escaped(&mut out, &truncate_chars(&short_type(field.data_type()), MAX_TYPE_CHARS));
        out.push_str("</td><td class=\"nullable\">");
        out.push_str(if field.is_nullable() { "yes" } else { "no" });
        out.push_str("</td></tr>\n");
    }
    out.push_str("</tbody></table></div>\n");
    if hidden_cols > 0 {
        push_more_columns_note(&mut out, hidden_cols);
    }
    out.push_str("</section>\n");

    // Data -------------------------------------------------------------------------------
    out.push_str("<section><h2>");
    if shown_rows == 0 {
        out.push_str("Rows</h2>\n<div class=\"empty\">");
        out.push_str(if total_rows == 0 {
            "This file contains no rows."
        } else {
            "No rows to show."
        });
        out.push_str("</div>\n");
    } else {
        let _ = writeln!(
            out,
            "First {}</h2>",
            plural(shown_rows as u64, "row", "rows")
        );
        if (shown_rows as u64) < total_rows {
            let _ = writeln!(
                out,
                "<div class=\"note\">Showing {} of {}.</div>",
                group_thousands(shown_rows as u64),
                plural(total_rows, "row", "rows")
            );
        }
        render_rows(&mut out, &fields[..shown_cols], batches);
        if hidden_cols > 0 {
            push_more_columns_note(&mut out, hidden_cols);
        }
    }
    out.push_str("</section>\n</body></html>\n");
    out
}

fn render_rows(out: &mut String, fields: &[std::sync::Arc<Field>], batches: &[RecordBatch]) {
    let numeric: Vec<bool> = fields.iter().map(|f| is_numeric(f.data_type())).collect();

    out.push_str("<div class=\"scroll data-scroll\"><table class=\"data\">\n<thead><tr><th class=\"num rowno\"></th>");
    for (field, &num) in fields.iter().zip(&numeric) {
        out.push_str(if num { "<th class=\"num\" title=\"" } else { "<th title=\"" });
        push_escaped(out, &short_type(field.data_type()));
        out.push_str("\">");
        push_escaped(out, field.name());
        out.push_str("</th>");
    }
    out.push_str("</tr></thead>\n<tbody>\n");

    let options = FormatOptions::new().with_null("null").with_display_error(true);
    let mut row_no = 0usize;
    let mut cell = String::new();
    for batch in batches {
        let columns: Vec<&dyn Array> = batch
            .columns()
            .iter()
            .take(fields.len())
            .map(|c| c.as_ref())
            .collect();
        let formatters: Vec<Option<ArrayFormatter<'_>>> = columns
            .iter()
            .map(|c| ArrayFormatter::try_new(*c, &options).ok())
            .collect();
        for r in 0..batch.num_rows() {
            let _ = write!(out, "<tr><td class=\"num rowno\">{row_no}</td>");
            for (c, array) in columns.iter().enumerate() {
                let num = numeric.get(c).copied().unwrap_or(false);
                if array.is_null(r) {
                    out.push_str(if num {
                        "<td class=\"num\"><span class=\"null\">null</span></td>"
                    } else {
                        "<td><span class=\"null\">null</span></td>"
                    });
                    continue;
                }
                cell.clear();
                let truncated = match &formatters[c] {
                    Some(fmt) => {
                        let mut w = LimitedWriter::new(&mut cell, MAX_CELL_CHARS);
                        let res = fmt.value(r).write(&mut w);
                        let over = w.overflowed;
                        if res.is_err() && !over {
                            cell.clear();
                            cell.push_str("<error>");
                        }
                        over
                    }
                    None => {
                        cell.push_str("<unsupported>");
                        false
                    }
                };
                out.push_str(if num { "<td class=\"num\">" } else { "<td>" });
                push_cell(out, &cell, truncated);
                out.push_str("</td>");
            }
            out.push_str("</tr>\n");
            row_no += 1;
        }
    }
    out.push_str("</tbody></table></div>\n");
}

fn push_more_columns_note(out: &mut String, hidden: usize) {
    let _ = writeln!(
        out,
        "<div class=\"note\">… and {} more {}</div>",
        group_thousands(hidden as u64),
        if hidden == 1 { "column" } else { "columns" }
    );
}

/// Write a cell value: HTML-escaped, newlines shown as "↵", tabs as spaces, and "…" appended
/// if it was truncated.
fn push_cell(out: &mut String, value: &str, truncated: bool) {
    let mut chars = 0usize;
    let mut cut = truncated;
    let mut iter = value.chars().peekable();
    while let Some(ch) = iter.next() {
        if chars >= MAX_CELL_CHARS {
            cut = true;
            break;
        }
        match ch {
            '\r' => {
                if iter.peek() == Some(&'\n') {
                    iter.next();
                }
                out.push_str("<span class=\"nl\">↵</span>");
            }
            '\n' => out.push_str("<span class=\"nl\">↵</span>"),
            '\t' => out.push(' '),
            c if c.is_control() => out.push('\u{FFFD}'),
            c => push_escaped_char(out, c),
        }
        chars += 1;
    }
    if cut {
        out.push('…');
    }
}

/// `fmt::Write` sink that stops (returning an error) once more than `limit` characters were
/// written, so huge values (big blobs, long lists) are never fully materialised.
struct LimitedWriter<'a> {
    buf: &'a mut String,
    remaining: usize,
    overflowed: bool,
}

impl<'a> LimitedWriter<'a> {
    fn new(buf: &'a mut String, limit: usize) -> Self {
        Self {
            buf,
            remaining: limit,
            overflowed: false,
        }
    }
}

impl fmt::Write for LimitedWriter<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for ch in s.chars() {
            if self.remaining == 0 {
                self.overflowed = true;
                return Err(fmt::Error);
            }
            self.buf.push(ch);
            self.remaining -= 1;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------------------------

fn compression_codecs(metadata: &ParquetMetaData) -> Vec<&'static str> {
    let Some(rg) = metadata.row_groups().first() else {
        return Vec::new();
    };
    let set: BTreeSet<&'static str> = rg
        .columns()
        .iter()
        .map(|c| codec_name(c.compression()))
        .collect();
    set.into_iter().collect()
}

fn codec_name(c: Compression) -> &'static str {
    match c {
        Compression::UNCOMPRESSED => "Uncompressed",
        Compression::SNAPPY => "Snappy",
        Compression::GZIP(_) => "Gzip",
        Compression::LZO => "LZO",
        Compression::BROTLI(_) => "Brotli",
        Compression::LZ4 => "LZ4",
        Compression::ZSTD(_) => "Zstd",
        Compression::LZ4_RAW => "LZ4 raw",
    }
}

fn is_numeric(dt: &DataType) -> bool {
    dt.is_numeric()
        || matches!(
            dt,
            DataType::Duration(_) | DataType::Decimal32(..) | DataType::Decimal64(..)
        )
        || matches!(dt, DataType::Dictionary(_, v) if v.is_numeric())
}

/// A compact, human-friendly rendering of an Arrow data type (drops list item field names
/// such as `element`/`item`, non-null annotations and metadata).
fn short_type(dt: &DataType) -> String {
    match dt {
        DataType::List(f) => format!("List({})", short_type(f.data_type())),
        DataType::LargeList(f) => format!("LargeList({})", short_type(f.data_type())),
        DataType::ListView(f) => format!("ListView({})", short_type(f.data_type())),
        DataType::LargeListView(f) => format!("LargeListView({})", short_type(f.data_type())),
        DataType::FixedSizeList(f, n) => format!("FixedSizeList({n} × {})", short_type(f.data_type())),
        DataType::Struct(fields) => {
            let inner: Vec<String> = fields
                .iter()
                .map(|f| format!("{}: {}", f.name(), short_type(f.data_type())))
                .collect();
            format!("Struct({})", inner.join(", "))
        }
        DataType::Map(entries, _) => match entries.data_type() {
            DataType::Struct(kv) if kv.len() == 2 => format!(
                "Map({}, {})",
                short_type(kv[0].data_type()),
                short_type(kv[1].data_type())
            ),
            other => format!("Map({})", short_type(other)),
        },
        DataType::Dictionary(k, v) => format!("Dictionary({}, {})", short_type(k), short_type(v)),
        DataType::RunEndEncoded(r, v) => format!(
            "RunEndEncoded({}, {})",
            short_type(r.data_type()),
            short_type(v.data_type())
        ),
        DataType::Timestamp(unit, tz) => {
            let unit = time_unit(unit);
            match tz {
                Some(tz) => format!("Timestamp({unit}, {tz})"),
                None => format!("Timestamp({unit})"),
            }
        }
        DataType::Time32(unit) => format!("Time32({})", time_unit(unit)),
        DataType::Time64(unit) => format!("Time64({})", time_unit(unit)),
        DataType::Duration(unit) => format!("Duration({})", time_unit(unit)),
        other => other.to_string(),
    }
}

fn time_unit(unit: &arrow::datatypes::TimeUnit) -> &'static str {
    use arrow::datatypes::TimeUnit;
    match unit {
        TimeUnit::Second => "s",
        TimeUnit::Millisecond => "ms",
        TimeUnit::Microsecond => "us",
        TimeUnit::Nanosecond => "ns",
    }
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_owned();
    }
    let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
    t.push('…');
    t
}

fn group_thousands(n: u64) -> String {
    let digits = n.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

fn plural(n: u64, one: &str, many: &str) -> String {
    format!("{} {}", group_thousands(n), if n == 1 { one } else { many })
}

/// Decimal (SI) byte formatting, like Finder: "512 bytes", "1.7 GB".
fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 6] = ["kB", "MB", "GB", "TB", "PB", "EB"];
    if bytes < 1000 {
        return if bytes == 1 {
            "1 byte".to_owned()
        } else {
            format!("{bytes} bytes")
        };
    }
    let mut value = bytes as f64 / 1000.0;
    let mut unit = 0;
    while value >= 999.95 && unit < UNITS.len() - 1 {
        value /= 1000.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

fn push_escaped(out: &mut String, s: &str) {
    for ch in s.chars() {
        push_escaped_char(out, ch);
    }
}

fn push_escaped_char(out: &mut String, ch: char) {
    match ch {
        '&' => out.push_str("&amp;"),
        '<' => out.push_str("&lt;"),
        '>' => out.push_str("&gt;"),
        '"' => out.push_str("&quot;"),
        '\'' => out.push_str("&#39;"),
        c => out.push(c),
    }
}

// ---------------------------------------------------------------------------------------------
// CSS
// ---------------------------------------------------------------------------------------------

const BASE_CSS: &str = r#"
:root {
  color-scheme: light dark;
  --bg: #ffffff; --fg: #1d1d1f; --muted: #6e6e73; --faint: #a1a1a6;
  --border: #e5e5ea; --header-bg: #f5f5f7; --stripe: #fafafa; --accent: #0a64d6;
  --type: #8a3ab9; --error: #c9342b;
}
@media (prefers-color-scheme: dark) {
  :root {
    --bg: #1e1e1e; --fg: #f2f2f7; --muted: #98989d; --faint: #636366;
    --border: #38383a; --header-bg: #2a2a2c; --stripe: #242426; --accent: #4ea1ff;
    --type: #d19bf2; --error: #ff6961;
  }
}
* { box-sizing: border-box; }
html, body { margin: 0; padding: 0; background: var(--bg); color: var(--fg); }
body {
  font: 13px/1.4 -apple-system, BlinkMacSystemFont, "SF Pro Text", "Helvetica Neue", sans-serif;
  -webkit-font-smoothing: antialiased;
}
"#;

const PREVIEW_CSS: &str = r#"
body { padding: 20px 24px 28px; }
header { margin-bottom: 18px; }
h1 { font-size: 20px; font-weight: 600; margin: 0 0 4px; overflow-wrap: anywhere; }
h2 { font-size: 13px; font-weight: 600; color: var(--muted); text-transform: uppercase;
     letter-spacing: 0.04em; margin: 22px 0 8px; }
.summary { color: var(--muted); font-size: 13px; }
.summary .sep { margin: 0 7px; color: var(--faint); }
.created { color: var(--faint); font-size: 12px; margin-top: 2px; overflow-wrap: anywhere; }
.note, .empty { color: var(--muted); font-size: 12px; margin: 6px 0; }
.empty { font-style: italic; }
.scroll { overflow: auto; border: 1px solid var(--border); border-radius: 8px; }
.schema-scroll { max-height: 320px; }
.data-scroll { max-height: calc(100vh - 48px); }
table { border-collapse: separate; border-spacing: 0; width: max-content; min-width: 100%; }
th, td { padding: 4px 10px; text-align: left; white-space: nowrap; vertical-align: top;
         border-bottom: 1px solid var(--border); }
tbody tr:last-child td { border-bottom: none; }
th { position: sticky; top: 0; z-index: 1; background: var(--header-bg); font-weight: 600;
     font-size: 12px; }
tbody tr:nth-child(even) td { background: var(--stripe); }
td { font-family: ui-monospace, "SF Mono", Menlo, monospace; font-size: 12px; }
td.name { font-weight: 500; }
td.type { color: var(--type); }
td.nullable { font-family: inherit; color: var(--muted); }
.num { text-align: right; font-variant-numeric: tabular-nums; }
td.rowno, td.idx, th.rowno { color: var(--faint); }
.schema td.name, .schema td.idx { font-family: -apple-system, BlinkMacSystemFont, sans-serif; font-size: 13px; }
.null { color: var(--faint); font-style: italic; }
.nl { color: var(--faint); }
"#;

const ERROR_CSS: &str = r#"
body { display: flex; align-items: center; justify-content: center; min-height: 100vh; padding: 32px; }
.error { max-width: 560px; text-align: center; }
.error-title { font-size: 17px; font-weight: 600; margin-bottom: 8px; }
.error-msg { color: var(--muted); font-size: 13px; overflow-wrap: anywhere; white-space: pre-wrap; }
"#;

#[cfg(test)]
mod tests;
