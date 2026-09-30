use super::*;

use std::ffi::CString;
use std::sync::Arc;

use arrow::array::{
    ArrayRef, Float64Array, Int32Array, Int64Array, ListArray, StringArray, StructArray,
    TimestampMicrosecondArray,
};
use arrow::datatypes::{Field, Int32Type, TimeUnit};
use parquet::arrow::ArrowWriter;
use parquet::file::properties::WriterProperties;

fn write_parquet(path: &Path, batch: &RecordBatch) {
    write_parquet_with(path, batch, None);
}

fn write_parquet_with(path: &Path, batch: &RecordBatch, props: Option<WriterProperties>) {
    let file = File::create(path).unwrap();
    let mut writer = ArrowWriter::try_new(file, batch.schema(), props).unwrap();
    writer.write(batch).unwrap();
    writer.close().unwrap();
}

fn mixed_batch() -> RecordBatch {
    let ids: ArrayRef = Arc::new(Int64Array::from(vec![Some(1), Some(2), None, Some(1_234_567)]));
    let floats: ArrayRef = Arc::new(Float64Array::from(vec![
        Some(1.5),
        Some(f64::NAN),
        None,
        Some(-0.25),
    ]));
    let long = "x".repeat(500);
    let strings: ArrayRef = Arc::new(StringArray::from(vec![
        Some("<script>alert('x')</script> & \"quoted\""),
        None,
        Some("line one\nline two"),
        Some(long.as_str()),
    ]));
    let ts: ArrayRef = Arc::new(TimestampMicrosecondArray::from(vec![
        Some(1_700_000_000_000_000),
        None,
        Some(0),
        Some(1_000_000),
    ]));
    let st: ArrayRef = Arc::new(StructArray::from(vec![
        (
            Arc::new(Field::new("a", DataType::Int32, true)),
            Arc::new(Int32Array::from(vec![1, 2, 3, 4])) as ArrayRef,
        ),
        (
            Arc::new(Field::new("b", DataType::Utf8, true)),
            Arc::new(StringArray::from(vec!["p", "q", "r", "s"])) as ArrayRef,
        ),
    ]));
    let list: ArrayRef = Arc::new(ListArray::from_iter_primitive::<Int32Type, _, _>(vec![
        Some(vec![Some(1), Some(2), Some(3)]),
        None,
        Some(vec![]),
        Some(vec![Some(4), None]),
    ]));
    RecordBatch::try_from_iter(vec![
        ("id", ids),
        ("score", floats),
        ("<b>&name", strings),
        ("ts", ts),
        ("nested", st),
        ("tags", list),
    ])
    .unwrap()
}

#[test]
fn renders_mixed_types() {
    let dir = tempfile::tempdir().unwrap();
    // `<` and `>` aren't allowed in Windows file names.
    let name = if cfg!(windows) { "mixed & 'odd'.parquet" } else { "mixed & <odd>.parquet" };
    let path = dir.path().join(name);
    write_parquet(&path, &mixed_batch());

    let html = preview_html(&path, 200).unwrap();

    // Document basics.
    assert!(html.starts_with("<!DOCTYPE html>"));
    assert!(html.contains("prefers-color-scheme: dark"));
    assert!(!html.contains("<script"), "raw <script> must never appear");
    assert!(!html.contains("http://") && !html.contains("https://"));

    // Header: escaped file name and summary.
    let heading = if cfg!(windows) { "<h1>mixed &amp; &#39;odd&#39;.parquet</h1>" } else { "<h1>mixed &amp; &lt;odd&gt;.parquet</h1>" };
    assert!(html.contains(heading), "file name is escaped");
    assert!(html.contains("4 rows"));
    assert!(html.contains("6 columns"));
    assert!(html.contains("1 row group"));
    assert!(html.contains("Created by parquet-rs"));
    assert!(html.contains(" bytes") || html.contains(" kB"));

    // Schema.
    assert!(html.contains("&lt;b&gt;&amp;name"));
    assert!(html.contains(">Int64<"));
    assert!(html.contains(">Float64<"));
    assert!(html.contains(">Utf8<"));
    assert!(html.contains(">Timestamp(us)<"));
    assert!(html.contains(">Struct(a: Int32, b: Utf8)<"));
    assert!(html.contains(">List(Int32)<"));

    // Data.
    assert!(html.contains("First 4 rows"));
    assert!(html.contains("&lt;script&gt;alert(&#39;x&#39;)&lt;/script&gt; &amp; &quot;quoted&quot;"));
    assert!(html.contains("<span class=\"null\">null</span>"));
    assert!(html.contains("NaN"));
    assert!(html.contains("1234567"));
    assert!(html.contains("line one<span class=\"nl\">↵</span>line two"));
    assert!(html.contains("2023-11-14T22:13:20"));
    assert!(html.contains("{a: 1, b: p}"));
    assert!(html.contains("[1, 2, 3]"));
    // Truncation: 120 chars then an ellipsis, never the full 500-char value.
    assert!(html.contains(&format!("{}…", "x".repeat(MAX_CELL_CHARS))));
    assert!(!html.contains(&"x".repeat(MAX_CELL_CHARS + 1)));
    // Numeric columns are right-aligned.
    assert!(html.contains("<th class=\"num\" title=\"Int64\">id</th>"));
    assert!(html.contains("<td class=\"num\">1.5</td>"));
    // 0-based row numbers.
    assert!(html.contains("<td class=\"num rowno\">0</td>"));
    assert!(html.contains("<td class=\"num rowno\">3</td>"));
    assert!(!html.contains("<td class=\"num rowno\">4</td>"));
}

#[test]
fn respects_max_rows_and_reports_totals() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("many.parquet");
    let n = 5_000i64;
    let batch = RecordBatch::try_from_iter(vec![(
        "v",
        Arc::new(Int64Array::from_iter_values(0..n)) as ArrayRef,
    )])
    .unwrap();
    // Several small row groups to exercise row-group pruning.
    let props = WriterProperties::builder().set_max_row_group_row_count(Some(1_000)).build();
    write_parquet_with(&path, &batch, Some(props));

    let html = preview_html(&path, 10).unwrap();
    assert!(html.contains("5,000 rows"));
    assert!(html.contains("5 row groups"));
    assert!(html.contains("First 10 rows"));
    assert!(html.contains("Showing 10 of 5,000 rows."));
    assert!(html.contains("<td class=\"num rowno\">9</td>"));
    assert!(!html.contains("<td class=\"num rowno\">10</td>"));

    // Reading across row-group boundaries.
    let html = preview_html(&path, 1_500).unwrap();
    assert!(html.contains("First 1,500 rows"));
    assert!(html.contains("<td class=\"num rowno\">1499</td><td class=\"num\">1499</td>"));
}

#[test]
fn wide_file_caps_columns() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("wide.parquet");
    let cols: Vec<(String, ArrayRef)> = (0..150)
        .map(|i| (format!("col_{i}"), Arc::new(Int32Array::from(vec![i, i + 1])) as ArrayRef))
        .collect();
    let batch = RecordBatch::try_from_iter(cols).unwrap();
    write_parquet(&path, &batch);

    let html = preview_html(&path, 200).unwrap();
    assert!(html.contains("150 columns"));
    assert!(html.contains(">col_99<"));
    assert!(!html.contains(">col_100<"));
    assert!(html.contains("and 50 more columns"));
}

#[test]
fn zero_row_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("empty.parquet");
    let batch = RecordBatch::try_from_iter(vec![(
        "v",
        Arc::new(Int64Array::from(Vec::<i64>::new())) as ArrayRef,
    )])
    .unwrap();
    write_parquet(&path, &batch);

    let html = preview_html(&path, 200).unwrap();
    assert!(html.contains("0 rows"));
    assert!(html.contains("This file contains no rows."));
    assert!(html.contains(">Int64<"));
}

#[test]
fn garbage_is_an_error() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("garbage.parquet");
    std::fs::write(&path, b"this is definitely not parquet, just some bytes <&>").unwrap();
    let err = preview_html(&path, 200).unwrap_err();
    assert!(err.contains("garbage.parquet"), "{err}");
    assert!(err.contains("not a readable Parquet file"), "{err}");

    let missing = preview_html(&dir.path().join("missing.parquet"), 200).unwrap_err();
    assert!(missing.contains("Could not open"), "{missing}");
}

#[test]
fn error_page_is_escaped() {
    let html = error_html("bad <script> & \"stuff\"");
    assert!(html.starts_with("<!DOCTYPE html>"));
    assert!(html.contains("bad &lt;script&gt; &amp; &quot;stuff&quot;"));
    assert!(!html.contains("<script"));
    assert!(html.contains("prefers-color-scheme: dark"));
}

#[test]
fn ffi_round_trip() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("ffi.parquet");
    write_parquet(&path, &mixed_batch());

    let c_path = CString::new(path.to_str().unwrap()).unwrap();
    let mut len = 0usize;
    let ptr = unsafe { parquetry_ql_preview(c_path.as_ptr(), 2, &mut len) };
    assert!(!ptr.is_null());
    assert!(len > 0);
    let html = unsafe { std::str::from_utf8(std::slice::from_raw_parts(ptr, len)) }
        .unwrap()
        .to_owned();
    unsafe { parquetry_ql_free(ptr, len) };
    assert!(html.contains("First 2 rows"));
    assert!(html.contains("Showing 2 of 4 rows."));

    // Garbage and null paths still produce an HTML error page.
    let bad = dir.path().join("bad.parquet");
    std::fs::write(&bad, b"nope").unwrap();
    let c_bad = CString::new(bad.to_str().unwrap()).unwrap();
    let mut len = 0usize;
    let ptr = unsafe { parquetry_ql_preview(c_bad.as_ptr(), 200, &mut len) };
    let html = unsafe { std::str::from_utf8(std::slice::from_raw_parts(ptr, len)) }
        .unwrap()
        .to_owned();
    unsafe { parquetry_ql_free(ptr, len) };
    assert!(html.contains("Can’t preview this file"));

    let mut len = 0usize;
    let ptr = unsafe { parquetry_ql_preview(std::ptr::null(), 200, &mut len) };
    assert!(!ptr.is_null());
    unsafe { parquetry_ql_free(ptr, len) };
    unsafe { parquetry_ql_free(std::ptr::null_mut(), 0) };
}

#[test]
fn formatting_helpers() {
    assert_eq!(group_thousands(0), "0");
    assert_eq!(group_thousands(999), "999");
    assert_eq!(group_thousands(1_000), "1,000");
    assert_eq!(group_thousands(1_234_567_890), "1,234,567,890");
    assert_eq!(format_bytes(512), "512 bytes");
    assert_eq!(format_bytes(1_700_000_000), "1.7 GB");
    assert_eq!(format_bytes(999_999), "1.0 MB");
    assert_eq!(format_bytes(20_000_000_000), "20.0 GB");
    let long = DataType::Struct(
        (0..20)
            .map(|i| Field::new(format!("field_{i}"), DataType::Int64, true))
            .collect(),
    );
    let t = truncate_chars(&short_type(&long), MAX_TYPE_CHARS);
    assert_eq!(t.chars().count(), MAX_TYPE_CHARS);
    assert!(t.ends_with('…'));
    assert_eq!(
        short_type(&DataType::Timestamp(TimeUnit::Nanosecond, Some("UTC".into()))),
        "Timestamp(ns, UTC)"
    );
}
