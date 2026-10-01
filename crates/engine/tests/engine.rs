mod common;

use std::time::{Duration, Instant};

use common::Fixture;
use parquetry_engine::*;

fn open(fx: &Fixture, location: &str) -> Dataset {
    Dataset::open(&fx.engine, SourceSpec::new(location))
        .wait()
        .unwrap_or_else(|e| panic!("open {location}: {e}"))
}

fn page(view: &View, rows: std::ops::Range<u64>) -> Page {
    view.fetch_page(PageRequest {
        rows,
        columns: 0..view.columns().len(),
    })
    .wait()
    .expect("page")
}

fn text(page: &Page, row: u64, col: usize) -> Option<String> {
    page.cell(row, col)
        .expect("cell in page")
        .as_ref()
        .map(|s| s.to_string())
}

fn col(ds: &Dataset, name: &str) -> usize {
    ds.column_index(name).unwrap_or_else(|| panic!("column {name}"))
}

// ---------------------------------------------------------------- opening

#[test]
fn opens_every_type() {
    let fx = Fixture::new();
    let path = fx.types_file();
    let ds = open(&fx, &path);
    assert_eq!(ds.row_count, 4);
    assert_eq!(ds.columns.len(), 19);
    assert_eq!(ds.format, Some(Format::Parquet));
    let kinds: Vec<ColumnKind> = ds.columns.iter().map(|c| c.kind).collect();
    assert!(kinds.contains(&ColumnKind::List));
    assert!(kinds.contains(&ColumnKind::Struct));
    assert!(kinds.contains(&ColumnKind::Map));
    assert!(kinds.contains(&ColumnKind::Uuid));
    assert!(kinds.contains(&ColumnKind::Binary));
    assert_eq!(ds.columns[col(&ds, "id")].physical_type.as_deref(), Some("INT64"));
    let summary = ds.parquet.as_ref().unwrap();
    assert_eq!(summary.row_groups, 1);
    assert!(summary.created_by.as_deref().unwrap_or("").contains("DuckDB"));

    let view = View::identity(&ds);
    let p = page(&view, 0..4);
    assert_eq!(p.row_count(), 4);
    assert_eq!(text(&p, 0, col(&ds, "txt")).as_deref(), Some("hello"));
    // Control characters become visible single-line symbols.
    assert_eq!(text(&p, 1, col(&ds, "txt")).as_deref(), Some("multi↵line→tab"));
    assert_eq!(text(&p, 2, col(&ds, "txt")), None);
    assert_eq!(text(&p, 1, col(&ds, "dbl")).as_deref(), Some("nan"));
    assert_eq!(text(&p, 0, col(&ds, "lst")).as_deref(), Some("[1, 2, 3]"));
    assert_eq!(text(&p, 0, col(&ds, "st")).as_deref(), Some("{'a': 1, 'b': x}"));
    assert_eq!(
        text(&p, 0, col(&ds, "ubig")).as_deref(),
        Some("18446744073709551615")
    );
    assert!(text(&p, 3, col(&ds, "txt")).unwrap().starts_with("😀"));

    // Full values keep newlines.
    let full = view
        .fetch_values(1..2, vec![col(&ds, "txt")], 10)
        .wait()
        .unwrap();
    assert_eq!(full[0][0].as_deref(), Some("multi\nline\ttab"));
    // JSON keeps types.
    let json = view
        .fetch_json(0..1, vec![col(&ds, "id"), col(&ds, "lst"), col(&ds, "st")], 10)
        .wait()
        .unwrap();
    assert_eq!(json[0], r#"{"id":1,"lst":[1,2,3],"st":{"a":1,"b":"x"}}"#);
}

#[test]
fn missing_and_corrupt_files_explain_themselves() {
    let fx = Fixture::new();
    let err = Dataset::open(&fx.engine, SourceSpec::new(fx.path_str("nope.parquet")))
        .wait()
        .unwrap_err();
    assert!(err.to_string().contains("doesn’t exist"), "{err}");

    let bad = fx.path_str("bad.parquet");
    std::fs::write(&bad, b"this is not parquet at all").unwrap();
    let err = Dataset::open(&fx.engine, SourceSpec::new(&bad)).wait().unwrap_err();
    assert!(!err.to_string().is_empty());
    assert!(!err.to_string().contains("LINE 1"), "raw SQL leaked: {err}");

    let empty_dir = fx.path("emptydir");
    std::fs::create_dir_all(&empty_dir).unwrap();
    let err = Dataset::open(&fx.engine, SourceSpec::new(empty_dir.to_string_lossy()))
        .wait()
        .unwrap_err();
    assert!(err.to_string().contains("No Parquet"), "{err}");
}

#[test]
fn empty_and_all_null_files() {
    let fx = Fixture::new();
    let path = fx.write("SELECT 1::INTEGER AS a, 'x' AS b WHERE false", "empty.parquet", "");
    let ds = open(&fx, &path);
    assert_eq!(ds.row_count, 0);
    let view = View::identity(&ds);
    assert_eq!(page(&view, 0..100).row_count(), 0);
    let stats = summarize(&view, vec![0, 1], StatsMode::Auto).wait().unwrap();
    assert_eq!(stats[0].rows, 0);
    assert!(stats[0].histogram.is_empty());
    let sorted = View::build(&ds, ViewSpec { sort: vec![SortKey::asc("a")], ..Default::default() })
        .wait()
        .unwrap();
    assert_eq!(sorted.row_count, 0);

    let path = fx.write("SELECT NULL::DOUBLE AS n FROM range(100)", "nulls.parquet", "");
    let ds = open(&fx, &path);
    let stats = summarize(&View::identity(&ds), vec![0], StatsMode::Auto).wait().unwrap();
    assert_eq!(stats[0].exact_nulls, Some(100));
    assert_eq!(stats[0].null_fraction(), 1.0);
    assert!(stats[0].histogram.is_empty());
}

#[test]
fn awkward_names_and_paths() {
    let fx = Fixture::new();
    let path = fx.write(
        r#"SELECT 1 AS "we""ird", 2 AS "with space", 3 AS "ünï", 4 AS "filename", 5 AS "file_row_number", 6 AS "select""#,
        "dir with 'quote's/my file.parquet",
        "",
    );
    let ds = open(&fx, &path);
    assert_eq!(ds.columns.len(), 6);
    assert_eq!(ds.columns[0].name, "we\"ird");
    let view = View::identity(&ds);
    let p = page(&view, 0..1);
    assert_eq!(text(&p, 0, 0).as_deref(), Some("1"));
    assert_eq!(text(&p, 0, 5).as_deref(), Some("6"));
    let filtered = View::build(
        &ds,
        ViewSpec {
            filters: vec![Filter::new("we\"ird", FilterOp::Equals, "1")],
            sort: vec![SortKey::desc("select")],
            ..Default::default()
        },
    )
    .wait()
    .expect("filter on awkward names");
    assert_eq!(filtered.row_count, 1);
    let p = page(&filtered, 0..1);
    assert_eq!(text(&p, 0, 3).as_deref(), Some("4"));
}

// ---------------------------------------------------------------- paging

#[test]
fn identity_pages_anywhere_in_a_large_file() {
    let fx = Fixture::new();
    let path = fx.big_file("big.parquet", 3_000_000, 100_000);
    let ds = open(&fx, &path);
    assert_eq!(ds.row_count, 3_000_000);
    assert_eq!(ds.parquet.as_ref().unwrap().row_groups, 30);
    let view = View::identity(&ds);
    for start in [0u64, 1_234_567, 2_999_990] {
        let t = Instant::now();
        let p = page(&view, start..start + 500);
        assert!(t.elapsed() < Duration::from_secs(2), "slow page at {start}");
        assert_eq!(p.rows.start, start);
        assert_eq!(text(&p, start, 0), Some(start.to_string()));
        assert_eq!(text(&p, start, 5), Some(format!("row {start}")));
    }
    // Past the end: clipped.
    let p = page(&view, 2_999_990..3_000_500);
    assert_eq!(p.row_count(), 10);
    // A column block.
    let p = view
        .fetch_page(PageRequest { rows: 10..20, columns: 2..4 })
        .wait()
        .unwrap();
    assert!(p.cell(10, 1).is_none());
    assert!(p.cell(10, 2).unwrap().is_some());
}

#[test]
fn sorted_and_filtered_views_match_duckdb() {
    let fx = Fixture::new();
    let path = fx.big_file("big.parquet", 400_000, 50_000);
    let ds = open(&fx, &path);
    let duck = fx.duck();
    let expected = |sql: &str| -> Vec<String> {
        let mut stmt = duck.prepare(sql).unwrap();
        stmt.query_map([], |r| r.get::<_, String>(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    };
    let spec = ViewSpec {
        filters: vec![
            Filter::new("color", FilterOp::Equals, "red"),
            Filter::new("amount", FilterOp::Greater, "500"),
        ],
        sort: vec![SortKey::desc("amount")],
        ..Default::default()
    };
    let view = View::build(&ds, spec).wait().unwrap();
    let want: Vec<String> = expected(&format!(
        "SELECT id::VARCHAR FROM '{path}' WHERE color = 'red' AND amount > 500 ORDER BY amount DESC NULLS LAST, id"
    ));
    assert_eq!(view.row_count as usize, want.len());
    for start in [0u64, 7_777, view.row_count - 50] {
        let p = page(&view, start..start + 50);
        for r in 0..50u64 {
            assert_eq!(
                text(&p, start + r, 0).as_deref(),
                Some(want[(start + r) as usize].as_str()),
                "row {}",
                start + r
            );
        }
    }

    // Materializing keeps exactly the same rows.
    let before = page(&view, 100..160);
    view.materialize_if_small().expect("small view").wait().unwrap();
    assert!(view.is_materialized());
    let after = page(&view, 100..160);
    for r in 100..160u64 {
        assert_eq!(text(&before, r, 0), text(&after, r, 0));
    }

    // Search across columns.
    let view = View::build(&ds, ViewSpec { search: "ROW 12345".into(), ..Default::default() })
        .wait()
        .unwrap();
    let want = expected(&format!(
        "SELECT id::VARCHAR FROM '{path}' WHERE contains(lower(txt), 'row 12345') ORDER BY id"
    ));
    assert_eq!(view.row_count as usize, want.len());
    let p = page(&view, 0..view.row_count);
    assert_eq!(text(&p, 0, 0).as_deref(), Some(want[0].as_str()));

    // Nulls sort last in both directions.
    let view = View::build(&ds, ViewSpec { sort: vec![SortKey::asc("maybe_null")], ..Default::default() })
        .wait()
        .unwrap();
    let p = page(&view, view.row_count - 1..view.row_count);
    assert_eq!(text(&p, view.row_count - 1, 4), None);

    // Custom WHERE, and helpful errors.
    let view = View::build(&ds, ViewSpec { where_sql: "id < 10 AND id % 2 = 0".into(), ..Default::default() })
        .wait()
        .unwrap();
    assert_eq!(view.row_count, 5);
    let err = View::build(&ds, ViewSpec { where_sql: "nope > 1".into(), ..Default::default() })
        .wait()
        .unwrap_err();
    assert!(err.to_string().to_lowercase().contains("nope"), "{err}");
    let err = View::build(
        &ds,
        ViewSpec { filters: vec![Filter::new("amount", FilterOp::Greater, "abc")], ..Default::default() },
    )
    .wait()
    .unwrap_err();
    assert!(err.to_string().contains("isn’t a valid float64"), "{err}");
}

#[test]
fn every_filter_operator_runs() {
    let fx = Fixture::new();
    let ds = open(&fx, &fx.types_file());
    for column in ds.columns.clone() {
        for op in FilterOp::for_kind(column.kind) {
            let value = match column.kind {
                ColumnKind::Integer | ColumnKind::Float | ColumnKind::Decimal => "1",
                ColumnKind::Date => "2024-01-02",
                ColumnKind::Timestamp => "2024-01-02 03:04:05",
                ColumnKind::Time => "03:04:05",
                ColumnKind::Interval => "3 days",
                ColumnKind::Boolean => "true",
                ColumnKind::Uuid => "6f1f5c3e-3f3e-4a9f-9f0e-1e2d3c4b5a69",
                _ => "a",
            };
            let filter = if op == FilterOp::Between {
                Filter::between(&column.name, value, value)
            } else {
                Filter::new(&column.name, op, value)
            };
            let result = View::build(&ds, ViewSpec { filters: vec![filter.clone()], ..Default::default() }).wait();
            assert!(result.is_ok(), "{} {:?}: {}", column.name, op, result.err().unwrap());
        }
    }
    // Sorting by every column works too.
    for column in ds.columns.clone() {
        for descending in [false, true] {
            let spec = ViewSpec { sort: vec![SortKey { column: column.name.clone(), descending }], ..Default::default() };
            let view = View::build(&ds, spec).wait().unwrap_or_else(|e| panic!("sort {}: {e}", column.name));
            assert_eq!(page(&view, 0..4).row_count(), 4);
        }
    }
}

// ---------------------------------------------------------------- multiple files

#[test]
fn folders_globs_and_hive_partitions() {
    let fx = Fixture::new();
    let base = fx.path_str("sales");
    fx.duck()
        .execute_batch(&format!(
            "COPY (SELECT i AS id, i * 1.5 AS amount, ['eu','us','apac'][1 + i % 3] AS region FROM range(30000) t(i))
             TO '{base}' (FORMAT parquet, PARTITION_BY (region))"
        ))
        .unwrap();
    // A hidden file and a stray non-data file must be ignored.
    std::fs::write(format!("{base}/_SUCCESS"), b"").unwrap();
    std::fs::write(format!("{base}/.DS_Store"), b"junk").unwrap();

    let ds = open(&fx, &base);
    assert_eq!(ds.row_count, 30_000);
    assert_eq!(ds.files.len(), 3);
    assert!(ds.column_index("region").is_some(), "hive column");
    let view = View::identity(&ds);
    let p = page(&view, 29_990..30_000);
    assert_eq!(p.row_count(), 10);

    let spec = ViewSpec { sort: vec![SortKey::desc("amount")], ..Default::default() };
    let sorted = View::build(&ds, spec).wait().unwrap();
    let p = page(&sorted, 0..3);
    assert_eq!(text(&p, 0, col(&ds, "id")).as_deref(), Some("29999"));
    assert_eq!(text(&p, 2, col(&ds, "id")).as_deref(), Some("29997"));
    let p = page(&sorted, 29_999..30_000);
    assert_eq!(text(&p, 29_999, col(&ds, "id")).as_deref(), Some("0"));

    let filtered = View::build(
        &ds,
        ViewSpec { filters: vec![Filter::new("region", FilterOp::Equals, "us")], ..Default::default() },
    )
    .wait()
    .unwrap();
    assert_eq!(filtered.row_count, 10_000);

    let glob = open(&fx, &format!("{base}/*/*.parquet"));
    assert_eq!(glob.row_count, 30_000);

    let stats = summarize(&view, vec![col(&ds, "region")], StatsMode::Auto).wait().unwrap();
    assert_eq!(stats[0].top_values.len(), 3);
}

#[test]
fn schema_evolution_across_files_is_merged() {
    let fx = Fixture::new();
    fx.write("SELECT 1 AS a, 'x' AS b", "evo/part1.parquet", "");
    fx.write("SELECT 2 AS a, 3.5 AS c", "evo/part2.parquet", "");
    let ds = open(&fx, &fx.path_str("evo"));
    assert_eq!(ds.row_count, 2);
    let names: Vec<&str> = ds.columns.iter().map(|c| c.name.as_str()).collect();
    assert_eq!(names, vec!["a", "b", "c"]);
    assert!(ds.notes.iter().any(|n| n.contains("different schemas")));
    let sorted = View::build(&ds, ViewSpec { sort: vec![SortKey::desc("a")], ..Default::default() })
        .wait()
        .unwrap();
    let p = page(&sorted, 0..2);
    assert_eq!(text(&p, 0, 2).as_deref(), Some("3.5"));
    assert_eq!(text(&p, 1, 1).as_deref(), Some("x"));
}

// ---------------------------------------------------------------- other formats

#[test]
fn csv_json_and_arrow() {
    let fx = Fixture::new();
    let csv = fx.path_str("people.csv");
    std::fs::write(&csv, "name,age,joined\nAda,36,2020-01-02\nBob,,2021-05-06\n\"Smith, J\",41,2019-12-31\n").unwrap();
    let ds = open(&fx, &csv);
    assert_eq!(ds.format, Some(Format::Csv));
    assert_eq!(ds.row_count, 3);
    assert_eq!(ds.columns[1].kind, ColumnKind::Integer);
    assert_eq!(ds.columns[2].kind, ColumnKind::Date);
    let sorted = View::build(&ds, ViewSpec { sort: vec![SortKey::desc("age")], ..Default::default() })
        .wait()
        .unwrap();
    let p = page(&sorted, 0..3);
    assert_eq!(text(&p, 0, 0).as_deref(), Some("Smith, J"));
    assert_eq!(text(&p, 2, 1), None);

    let json = fx.path_str("events.jsonl");
    std::fs::write(&json, "{\"id\": 1, \"tags\": [\"a\"], \"meta\": {\"k\": 1}}\n{\"id\": 2, \"tags\": [], \"meta\": {\"k\": 2}}\n").unwrap();
    let ds = open(&fx, &json);
    assert_eq!(ds.format, Some(Format::Json));
    assert_eq!(ds.row_count, 2);
    assert_eq!(ds.columns[1].kind, ColumnKind::List);

    // Arrow IPC file written with the arrow crate.
    let arrow_path = fx.path_str("data.arrow");
    {
        use std::sync::Arc;
        use arrow::array::{Int64Array, StringArray};
        use arrow::datatypes::{DataType, Field, Schema};
        use arrow::record_batch::RecordBatch;
        let schema = Arc::new(Schema::new(vec![
            Field::new("n", DataType::Int64, false),
            Field::new("s", DataType::Utf8, true),
        ]));
        let file = std::fs::File::create(&arrow_path).unwrap();
        let mut writer = arrow::ipc::writer::FileWriter::try_new(file, &schema).unwrap();
        for chunk in 0..3 {
            let batch = RecordBatch::try_new(
                schema.clone(),
                vec![
                    Arc::new(Int64Array::from(vec![chunk * 2, chunk * 2 + 1])),
                    Arc::new(StringArray::from(vec![Some("a"), None])),
                ],
            )
            .unwrap();
            writer.write(&batch).unwrap();
        }
        writer.finish().unwrap();
    }
    let ds = open(&fx, &arrow_path);
    assert_eq!(ds.format, Some(Format::Arrow));
    assert_eq!(ds.row_count, 6);
    let p = page(&View::identity(&ds), 0..6);
    assert_eq!(text(&p, 5, 0).as_deref(), Some("5"));

    // Unknown extension: sniffed.
    let sniff = fx.path_str("mystery.bin");
    std::fs::copy(fx.types_file(), &sniff).unwrap();
    assert_eq!(open(&fx, &sniff).format, Some(Format::Parquet));
}

// ---------------------------------------------------------------- statistics

#[test]
fn exact_statistics() {
    let fx = Fixture::new();
    let path = fx.big_file("stats.parquet", 100_000, 10_000);
    let ds = open(&fx, &path);
    let view = View::identity(&ds);
    let all: Vec<usize> = (0..ds.columns.len()).collect();
    let stats = summarize(&view, all, StatsMode::Auto).wait().unwrap();
    let by_name = |n: &str| stats.iter().find(|s| s.column == n).unwrap();

    let id = by_name("id");
    assert!(!id.sampled);
    assert_eq!(id.min.as_deref(), Some("0"));
    assert_eq!(id.max.as_deref(), Some("99999"));
    assert_eq!(id.histogram.len(), 20);
    assert_eq!(id.histogram.iter().map(|b| b.count).sum::<u64>(), 100_000);
    assert!((id.mean.unwrap() - 49_999.5).abs() < 1e-6);

    let maybe = by_name("maybe_null");
    assert_eq!(maybe.exact_nulls, Some(10_000));
    assert!((maybe.null_fraction() - 0.1).abs() < 1e-9);

    let color = by_name("color");
    assert_eq!(color.preferred_chart(), ChartKind::TopValues);
    assert_eq!(color.top_values.len(), 5);
    assert_eq!(color.top_values.iter().map(|v| v.count).sum::<u64>(), 100_000);
    assert!(color.text_length.is_some());

    let ts = by_name("ts");
    assert!(ts.histogram_is_time);
    assert_eq!(ts.histogram.iter().map(|b| b.count).sum::<u64>(), 100_000);
    assert!(ts.quantiles.is_some());

    let txt = by_name("txt");
    assert!(txt.is_unique());
    assert_eq!(txt.preferred_chart(), ChartKind::None);
}

#[test]
fn sampled_statistics_use_footers_for_exact_counts() {
    let settings = EngineSettings { sample_threshold_rows: 100_000, remote_sample_threshold_rows: 100_000, sample_rows: 20_000, ..Default::default() };
    let fx = Fixture::with_settings(settings);
    let path = fx.big_file("sampled.parquet", 1_000_000, 50_000);
    let ds = open(&fx, &path);
    let view = View::identity(&ds);
    let stats = summarize(&view, vec![0, 4], StatsMode::Auto).wait().unwrap();
    let id = &stats[0];
    assert!(id.sampled);
    assert!(id.scanned_rows >= 15_000 && id.scanned_rows <= 25_000, "{}", id.scanned_rows);
    assert_eq!(id.histogram.len(), 20);
    assert!(id.min_max_exact);
    assert_eq!(id.max.as_deref(), Some("999999"));
    // Even spacing: the sample spans the whole file.
    let hist_total: u64 = id.histogram.iter().map(|b| b.count).sum();
    assert_eq!(hist_total, id.scanned_rows);
    assert!(id.histogram.iter().filter(|b| b.count > 0).count() >= 10);
    let maybe = &stats[1];
    assert_eq!(maybe.exact_nulls, Some(100_000));

    let exact = summarize(&view, vec![0], StatsMode::Exact).wait().unwrap();
    assert!(!exact[0].sampled);
    assert_eq!(exact[0].scanned_rows, 1_000_000);

    // Filtered view of a large dataset.
    let filtered = View::build(&ds, ViewSpec { filters: vec![Filter::new("color", FilterOp::Equals, "red")], ..Default::default() })
        .wait()
        .unwrap();
    let stats = summarize(&filtered, vec![2], StatsMode::Auto).wait().unwrap();
    assert_eq!(stats[0].top_values.len(), 1);
    assert_eq!(stats[0].top_values[0].value.as_deref(), Some("red"));
}

#[test]
fn statistics_for_every_type() {
    let fx = Fixture::new();
    let ds = open(&fx, &fx.types_file());
    let all: Vec<usize> = (0..ds.columns.len()).collect();
    let stats = summarize(&View::identity(&ds), all, StatsMode::Auto).wait().unwrap();
    assert_eq!(stats.len(), ds.columns.len());
    for s in &stats {
        assert_eq!(s.rows, 4, "{}", s.column);
    }
}

// ---------------------------------------------------------------- metadata, SQL, export, diff

#[test]
fn metadata_tables() {
    let fx = Fixture::new();
    let path = fx.big_file("meta.parquet", 50_000, 10_000);
    let ds = open(&fx, &path);
    for which in MetadataTable::all() {
        let table = metadata_table(&ds, *which).wait().unwrap_or_else(|e| panic!("{which:?}: {e}"));
        match which {
            MetadataTable::ColumnStorage => assert_eq!(table.row_count, 6),
            MetadataTable::RowGroups => assert_eq!(table.row_count, 5),
            MetadataTable::ColumnChunks => assert_eq!(table.row_count, 30),
            MetadataTable::Files => assert_eq!(table.row_count, 1),
            MetadataTable::Schema => assert!(table.row_count >= 7),
            MetadataTable::KeyValue => {}
        }
        let view = View::identity(&table);
        assert_eq!(page(&view, 0..table.row_count).row_count() as u64, table.row_count);
    }
    let csv = fx.path_str("x.csv");
    std::fs::write(&csv, "a\n1\n").unwrap();
    assert!(metadata_table(&open(&fx, &csv), MetadataTable::RowGroups).wait().is_err());
}

#[test]
fn sql_queries() {
    let fx = Fixture::new();
    let path = fx.big_file("q.parquet", 10_000, 5_000);
    let ds = open(&fx, &path);
    let tables = vec![SqlTable { name: "t".into(), dataset: ds.clone() }];
    let out = run_sql(&fx.engine, "SELECT color, count(*) AS n FROM t GROUP BY 1 ORDER BY 1".into(), tables.clone())
        .wait()
        .unwrap();
    let result = out.result.unwrap();
    assert_eq!(result.row_count, 5);
    assert_eq!(result.columns[1].name, "n");
    let p = page(&View::identity(&result), 0..5);
    assert_eq!(text(&p, 0, 0).as_deref(), Some("blue"));

    // Multiple statements; the last one's rows are kept.
    let out = run_sql(&fx.engine, "SET threads = 4; SELECT 42 AS answer;".into(), vec![]).wait().unwrap();
    assert_eq!(out.result.unwrap().row_count, 1);
    // Statements without rows.
    let out = run_sql(&fx.engine, "SET threads = 8".into(), vec![]).wait().unwrap();
    assert!(out.result.is_none());
    assert_eq!(out.message.as_deref(), Some("Done"));
    // DESCRIBE and SUMMARIZE work as results.
    let out = run_sql(&fx.engine, "SUMMARIZE t".into(), tables.clone()).wait().unwrap();
    assert_eq!(out.result.unwrap().row_count, 6);
    // Errors are readable.
    let err = run_sql(&fx.engine, "SELECT nope FROM t".into(), tables.clone()).wait().unwrap_err();
    assert!(err.to_string().contains("nope"), "{err}");
    let err = run_sql(&fx.engine, "SELEC 1".into(), vec![]).wait().unwrap_err();
    assert!(err.to_string().to_lowercase().contains("syntax"), "{err}");
    // Reading files directly.
    let out = run_sql(&fx.engine, format!("SELECT count(*) FROM '{path}'"), vec![]).wait().unwrap();
    assert_eq!(out.result.unwrap().row_count, 1);
    // Result sets can be filtered and sorted like any dataset.
    let out = run_sql(&fx.engine, "SELECT * FROM t".into(), tables).wait().unwrap();
    let result = out.result.unwrap();
    let sorted = View::build(&result, ViewSpec { sort: vec![SortKey::desc("id")], ..Default::default() })
        .wait()
        .unwrap();
    assert_eq!(text(&page(&sorted, 0..1), 0, 0).as_deref(), Some("9999"));
}

#[test]
fn sql_result_limit() {
    let settings = EngineSettings { result_limit_rows: 1000, ..Default::default() };
    let fx = Fixture::with_settings(settings);
    let out = run_sql(&fx.engine, "SELECT * FROM range(5000)".into(), vec![]).wait().unwrap();
    assert!(out.truncated);
    assert_eq!(out.result.unwrap().row_count, 1000);
}

#[test]
fn exports_round_trip() {
    let fx = Fixture::new();
    let path = fx.big_file("e.parquet", 20_000, 5_000);
    let ds = open(&fx, &path);
    let view = View::build(
        &ds,
        ViewSpec {
            filters: vec![Filter::new("color", FilterOp::Equals, "blue")],
            sort: vec![SortKey::asc("amount")],
            ..Default::default()
        },
    )
    .wait()
    .unwrap();
    for format in ExportFormat::all() {
        let out_path = fx.path_str(&format!("out.{}", format.extension()));
        let outcome = export_view(&view, Some(vec![0, 1]), out_path.clone(), *format).wait().unwrap();
        assert_eq!(outcome.rows, view.row_count, "{format:?}");
        let reopened = open(&fx, &out_path);
        assert_eq!(reopened.row_count, view.row_count, "{format:?}");
        assert_eq!(reopened.columns.len(), 2, "{format:?}");
    }
    // Order is preserved.
    let reopened = open(&fx, &fx.path_str("out.parquet"));
    let first = page(&View::identity(&reopened), 0..2);
    let expected = page(&view, 0..2);
    assert_eq!(text(&first, 0, 0), text(&expected, 0, 0));
}

#[test]
fn comparisons() {
    let fx = Fixture::new();
    let a = fx.write("SELECT i AS id, i * 2 AS v, 'x' AS only_a FROM range(100) t(i)", "a.parquet", "");
    let b = fx.write(
        "SELECT i AS id, CASE WHEN i = 5 THEN -1 ELSE i * 2 END AS v, 'y' AS only_b FROM range(1, 101) t(i)",
        "b.parquet",
        "",
    );
    let (a, b) = (open(&fx, &a), open(&fx, &b));
    let keyed = compare(&a, &b, CompareOptions { keys: vec!["id".into()] }).wait().unwrap();
    assert_eq!(keyed.only_left.row_count, 1);
    assert_eq!(keyed.only_right.row_count, 1);
    let changed = keyed.changed.as_ref().unwrap();
    assert_eq!(changed.row_count, 1);
    assert_eq!(keyed.column_changes.len(), 1);
    assert_eq!(keyed.column_changes[0].column, "v");
    assert_eq!(keyed.only_left_columns[0].name, "only_a");
    assert_eq!(keyed.only_right_columns[0].name, "only_b");
    assert!(!keyed.is_identical());

    let unkeyed = compare(&a, &b, CompareOptions::default()).wait().unwrap();
    assert_eq!(unkeyed.only_left.row_count, 2);
    assert_eq!(unkeyed.only_right.row_count, 2);

    let same = compare(&a, &a, CompareOptions::default()).wait().unwrap();
    assert!(same.is_identical());
    assert!(compare(&a, &b, CompareOptions { keys: vec!["only_a".into()] }).wait().is_err());
}

// ---------------------------------------------------------------- cancellation and concurrency

#[test]
fn cancelling_interrupts_and_workers_recover() {
    let fx = Fixture::new();
    let slow = fx.engine.run(Lane::Task, |conn| {
        let n: i64 = conn.query_row(
            "SELECT count(*) FROM range(100000000) a, range(100000) b WHERE a.range + b.range = -1",
            [],
            |r| r.get(0),
        )?;
        Ok(n)
    });
    std::thread::sleep(Duration::from_millis(300));
    let started = Instant::now();
    slow.cancel();
    let result = slow.wait();
    assert!(matches!(result, Err(Error::Cancelled)), "{result:?}");
    assert!(started.elapsed() < Duration::from_secs(3));
    // Every Task worker still works afterwards.
    for _ in 0..6 {
        let n = fx
            .engine
            .run(Lane::Task, |conn| Ok(conn.query_row("SELECT 41 + 1", [], |r| r.get::<_, i64>(0))?))
            .wait()
            .unwrap();
        assert_eq!(n, 42);
    }
    // Dropping a queued job skips it.
    let flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let blockers: Vec<_> = (0..2)
        .map(|_| fx.engine.run(Lane::Task, |_| { std::thread::sleep(Duration::from_millis(300)); Ok(()) }))
        .collect();
    let f = flag.clone();
    drop(fx.engine.run(Lane::Task, move |_| { f.store(true, std::sync::atomic::Ordering::SeqCst); Ok(()) }));
    for b in blockers {
        b.wait().unwrap();
    }
    std::thread::sleep(Duration::from_millis(100));
    assert!(!flag.load(std::sync::atomic::Ordering::SeqCst));
}

#[test]
fn many_concurrent_page_requests() {
    let fx = Fixture::new();
    let path = fx.big_file("c.parquet", 500_000, 25_000);
    let ds = open(&fx, &path);
    let view = View::build(&ds, ViewSpec { sort: vec![SortKey::asc("amount")], ..Default::default() })
        .wait()
        .unwrap();
    let jobs: Vec<_> = (0..40u64)
        .map(|i| view.fetch_page(PageRequest { rows: i * 10_000..i * 10_000 + 200, columns: 0..6 }))
        .collect();
    for (i, job) in jobs.into_iter().enumerate() {
        let p = job.wait().unwrap();
        assert_eq!(p.rows.start, i as u64 * 10_000);
        assert_eq!(p.row_count(), 200);
    }
}

#[test]
fn datasets_clean_up_their_tables() {
    let fx = Fixture::new();
    let path = fx.big_file("clean.parquet", 1000, 500);
    {
        let ds = open(&fx, &path);
        let _v = View::build(&ds, ViewSpec { sort: vec![SortKey::asc("id")], ..Default::default() })
            .wait()
            .unwrap();
    }
    std::thread::sleep(Duration::from_millis(300));
    let remaining = fx
        .engine
        .run(Lane::Task, |conn| {
            Ok(conn.query_row(
                "SELECT (SELECT count(*) FROM duckdb_tables() WHERE table_name LIKE 'pq_%') + (SELECT count(*) FROM duckdb_views() WHERE view_name LIKE 'pq_%')",
                [],
                |r| r.get::<_, i64>(0),
            )?)
        })
        .wait()
        .unwrap();
    let names = fx
        .engine
        .run(Lane::Task, |conn| {
            let mut stmt = conn.prepare("SELECT table_name FROM duckdb_tables() WHERE table_name LIKE 'pq_%' UNION ALL SELECT view_name FROM duckdb_views() WHERE view_name LIKE 'pq_%'")?;
            Ok(stmt.query_map([], |r| r.get::<_, String>(0))?.map(|r| r.unwrap()).collect::<Vec<_>>())
        })
        .wait()
        .unwrap();
    assert_eq!(remaining, 0, "{names:?}");
}

#[test]
fn filtered_view_statistics_small_and_large() {
    let settings = EngineSettings { materialize_limit_rows: 10_000, sample_rows: 5_000, ..Default::default() };
    let fx = common::Fixture::with_settings(settings);
    let path = fx.big_file("fv.parquet", 200_000, 20_000);
    let ds = open(&fx, &path);
    // Small result: copied into memory, exact statistics.
    let small = View::build(&ds, ViewSpec { filters: vec![Filter::new("id", FilterOp::Less, "5000")], ..Default::default() })
        .wait()
        .unwrap();
    let stats = summarize(&small, vec![0], StatsMode::Auto).wait().unwrap();
    assert!(!stats[0].sampled);
    assert!(small.is_materialized());
    assert_eq!(stats[0].max.as_deref(), Some("4999"));
    // Large result: a random sample of the matching rows.
    let large = View::build(&ds, ViewSpec { filters: vec![Filter::new("color", FilterOp::NotEquals, "red")], ..Default::default() })
        .wait()
        .unwrap();
    let stats = summarize(&large, vec![2], StatsMode::Auto).wait().unwrap();
    assert!(stats[0].sampled);
    assert_eq!(stats[0].scanned_rows, 5_000);
    assert_eq!(stats[0].top_values.len(), 4);
    assert!(stats[0].top_values.iter().all(|v| v.value.as_deref() != Some("red")));
    let exact = summarize(&large, vec![2], StatsMode::Exact).wait().unwrap();
    assert!(!exact[0].sampled);
    assert_eq!(exact[0].scanned_rows, large.row_count);
}

#[test]
fn distinct_counts_are_exact_and_never_exceed_rows() {
    let fx = Fixture::new();
    let path = fx.big_file("d.parquet", 300_000, 50_000);
    let ds = open(&fx, &path);
    let stats = summarize(&View::identity(&ds), vec![0, 2, 4], StatsMode::Auto).wait().unwrap();
    assert_eq!(stats[0].distinct, Some(300_000));
    assert!(stats[0].distinct_exact);
    assert_eq!(stats[1].distinct, Some(5));
    assert_eq!(stats[2].distinct, Some(900)); // values i % 1000 where i % 10 != 0
}

#[test]
fn chart_bars_filter_to_exactly_their_rows() {
    let fx = Fixture::new();
    let path = fx.write(
        "SELECT i AS n,
                (i * 0.37) % 91.3 AS f,
                CAST((i % 700) / 7.0 AS DECIMAL(10,2)) AS d,
                DATE '2020-01-01' + CAST(i % 900 AS INTEGER) AS day,
                TIMESTAMP '2024-03-01 00:00:00' + to_microseconds(CAST(i AS BIGINT) * 7_777_777) AS ts,
                CASE WHEN i % 11 = 0 THEN NULL ELSE ['red', 'green', 'blue', 'amber, dark'][1 + i % 4] END AS color,
                CASE i % 3 WHEN 0 THEN 'x' WHEN 1 THEN 'y' ELSE NULL END AS small
         FROM range(20000) r(i)",
        "bars.parquet",
        "",
    );
    let ds = open(&fx, &path);
    let columns: Vec<usize> = (0..ds.columns.len()).collect();
    let summaries = summarize(&View::identity(&ds), columns, StatsMode::Exact).wait().unwrap();
    let count = |filters: Vec<Filter>| View::build(&ds, ViewSpec { filters, ..Default::default() }).wait().unwrap().row_count;

    for summary in &summaries {
        match summary.preferred_chart() {
            ChartKind::Histogram => {
                let mut total = 0;
                for (ix, bin) in summary.histogram.iter().enumerate() {
                    let Some(filters) = summary.filters_for_bar(ix) else {
                        assert_eq!(bin.count, 0, "{} bin {ix} has rows but no filters", summary.column);
                        continue;
                    };
                    let n = count(filters.clone());
                    total += n;
                    if summary.kind == ColumnKind::Float || summary.kind == ColumnKind::Decimal {
                        // Edges are rounded to readable values: allow a sliver of drift.
                        let slack = (bin.count / 50).max(2);
                        assert!(n.abs_diff(bin.count) <= slack, "{} bin {ix}: {n} rows, bar says {} ({filters:?})", summary.column, bin.count);
                    } else {
                        assert_eq!(n, bin.count, "{} bin {ix} ({filters:?})", summary.column);
                    }
                }
                // Bins partition the non-null rows.
                assert_eq!(total, summary.rows - summary.scanned_nulls, "{}", summary.column);
            }
            ChartKind::TopValues => {
                let shown: u64 = summary.top_values.iter().map(|v| v.count).sum();
                for (ix, value) in summary.top_values.iter().enumerate() {
                    let filters = summary.filters_for_bar(ix).unwrap();
                    assert_eq!(count(filters), value.count, "{} = {:?}", summary.column, value.value);
                }
                if let Some(filters) = summary.filters_for_bar(summary.top_values.len()) {
                    assert_eq!(count(filters), summary.rows - shown, "{} other", summary.column);
                }
            }
            ChartKind::None => {}
        }
    }
    let kind = |name: &str| summaries.iter().find(|s| s.column == name).unwrap().preferred_chart();
    for name in ["n", "f", "d", "day", "ts"] {
        assert_eq!(kind(name), ChartKind::Histogram, "{name}");
    }
    assert_eq!(kind("color"), ChartKind::TopValues);
    assert_eq!(kind("small"), ChartKind::TopValues);
    // "amber, dark" can't go in a comma-separated list, so "other" has no filter;
    // the null bar does.
    let color = summaries.iter().find(|s| s.column == "color").unwrap();
    let null_bar = color.top_values.iter().position(|v| v.value.is_none()).unwrap();
    assert_eq!(color.filters_for_bar(null_bar).unwrap(), vec![Filter::new("color", FilterOp::IsNull, "")]);
}

#[test]
fn value_counts_are_complete_exact_and_searchable() {
    let fx = Fixture::new();
    let path = fx.write(
        "SELECT i AS id, CASE WHEN i % 10 = 0 THEN NULL ELSE 'v' || (i % 37) END AS tag FROM range(50000) r(i)",
        "vc.parquet",
        "",
    );
    let ds = open(&fx, &path);
    let tag = col(&ds, "tag");
    let all = value_counts(&View::identity(&ds), tag, "", 1000).wait().unwrap();
    let duck = fx.duck();
    let distinct: i64 = duck.query_row(&format!("SELECT count(DISTINCT coalesce(tag, '<null>')) FROM '{path}'"), [], |r| r.get(0)).unwrap();
    assert_eq!(all.distinct, distinct as u64, "every value, null included");
    assert_eq!(all.values.len(), distinct as usize);
    assert_eq!(all.matching_rows, 50_000);
    assert_eq!(all.rows, 50_000);
    assert_eq!(all.values.iter().map(|v| v.count).sum::<u64>(), 50_000);
    assert!(all.values.windows(2).all(|w| w[0].count >= w[1].count), "most frequent first");
    let nulls = all.values.iter().find(|v| v.value.is_none()).unwrap();
    assert_eq!(nulls.count, 5_000);

    // A limit keeps the most frequent but still reports the totals.
    let top = value_counts(&View::identity(&ds), tag, "", 5).wait().unwrap();
    assert_eq!(top.values.len(), 5);
    assert_eq!(top.distinct, all.distinct);
    assert_eq!(top.values[..], all.values[..5]);

    // Search is case-insensitive, over all values (not just the top).
    let found = value_counts(&View::identity(&ds), tag, " V3 ", 1000).wait().unwrap();
    let mut names: Vec<String> = found.values.iter().filter_map(|v| v.value.clone()).collect();
    names.sort();
    assert_eq!(names, ["v3", "v30", "v31", "v32", "v33", "v34", "v35", "v36"]);
    assert_eq!(found.distinct, 8);
    assert_eq!(found.matching_rows, found.values.iter().map(|v| v.count).sum::<u64>());

    // Filtered views count only their rows.
    let filtered = View::build(&ds, ViewSpec { filters: vec![Filter::new("id", FilterOp::Less, "100")], ..Default::default() })
        .wait()
        .unwrap();
    let counts = value_counts(&filtered, tag, "", 1000).wait().unwrap();
    assert_eq!(counts.rows, 100);
    assert_eq!(counts.values.iter().map(|v| v.count).sum::<u64>(), 100);
    let none = value_counts(&filtered, tag, "zzz", 1000).wait().unwrap();
    assert_eq!((none.values.len(), none.distinct, none.matching_rows), (0, 0, 0));
}

#[test]
fn selection_stats_match_duckdb() {
    let fx = Fixture::new();
    let path = fx.write(
        "SELECT i AS id, CASE WHEN i % 5 = 0 THEN NULL ELSE i * 0.5 END AS x, 'r' || (i % 3) AS label FROM range(10000) r(i)",
        "sel.parquet",
        "",
    );
    let ds = open(&fx, &path);
    let (id, x, label) = (col(&ds, "id"), col(&ds, "x"), col(&ds, "label"));
    let view = View::identity(&ds);
    let close = |a: f64, b: f64| (a - b).abs() < 1e-6 * b.abs().max(1.0);

    // Rows 100..200 of x: nulls skipped.
    let s = view.selection_stats(100..200, vec![x]).wait().unwrap().unwrap();
    let want: Vec<f64> = (100..200).filter(|i| i % 5 != 0).map(|i| i as f64 * 0.5).collect();
    assert_eq!((s.cells, s.values, s.numbers), (100, 80, 80));
    assert!(close(s.sum, want.iter().sum()));
    assert!(close(s.mean().unwrap(), want.iter().sum::<f64>() / 80.0));
    assert_eq!((s.min, s.max), (Some(50.5), Some(99.5)));

    // Two numeric columns and a text one: text counts as values, not numbers.
    let s = view.selection_stats(0..10, vec![id, x, label]).wait().unwrap().unwrap();
    assert_eq!((s.cells, s.values, s.numbers), (30, 28, 18));
    assert!(close(s.sum, 45.0 + 20.0)); // x: (1+2+3+4+6+7+8+9) * 0.5
    assert_eq!((s.min, s.max), (Some(0.0), Some(9.0)));

    // The whole view reads the source; a sorted view keeps the same totals.
    let all = view.selection_stats(0..10_000, vec![id]).wait().unwrap().unwrap();
    assert!(close(all.sum, (0..10_000).sum::<i64>() as f64));
    let sorted = View::build(&ds, ViewSpec { sort: vec![SortKey::desc("id")], ..Default::default() }).wait().unwrap();
    let first = sorted.selection_stats(0..3, vec![id]).wait().unwrap().unwrap();
    assert!(close(first.sum, (9_999 + 9_998 + 9_997) as f64), "the sorted view's first rows");
    let whole = sorted.selection_stats(0..10_000, vec![id]).wait().unwrap().unwrap();
    assert!(close(whole.sum, all.sum));

    // Nothing numeric: no sum or mean.
    let text = view.selection_stats(0..10, vec![label]).wait().unwrap().unwrap();
    assert_eq!((text.numbers, text.mean(), text.min), (0, None, None));
}
