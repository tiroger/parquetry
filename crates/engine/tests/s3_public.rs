//! Network tests against public S3 data. Run with `cargo test -- --ignored`.

mod common;

use parquetry_engine::*;

fn anonymous() -> common::Fixture {
    let settings = EngineSettings {
        s3: S3Settings { anonymous: true, ..Default::default() },
        ..Default::default()
    };
    common::Fixture::with_settings(settings)
}

const OOKLA: &str = "s3://ookla-open-data/parquet/performance/type=fixed/year=2023/quarter=1/";

#[test]
#[ignore]
fn list_and_open_public_s3() {
    let fx = anonymous();
    let entries = fx.engine.s3_list(OOKLA.into()).wait().expect("list");
    assert!(!entries.is_empty());
    let file = entries.iter().find(|e| e.name.ends_with(".parquet")).expect("a parquet object");
    let t = std::time::Instant::now();
    let ds = Dataset::open(&fx.engine, SourceSpec::new(&file.url)).wait().expect("open");
    eprintln!("opened {} rows in {:?}", ds.row_count, t.elapsed());
    assert!(ds.row_count > 1000);
    let view = View::identity(&ds);
    let t = std::time::Instant::now();
    let page = view
        .fetch_page(PageRequest { rows: ds.row_count / 2..ds.row_count / 2 + 200, columns: 0..ds.columns.len() })
        .wait()
        .expect("page");
    eprintln!("middle page in {:?}", t.elapsed());
    assert_eq!(page.row_count(), 200);
    let t = std::time::Instant::now();
    let stats = summarize(&view, (0..ds.columns.len()).collect(), StatsMode::Auto).wait().expect("stats");
    eprintln!("stats in {:?} (sampled: {})", t.elapsed(), stats[0].sampled);

    // Folder open (partition prefix) resolves to the parquet object(s).
    let folder = Dataset::open(&fx.engine, SourceSpec::new(OOKLA)).wait().expect("folder");
    assert_eq!(folder.row_count, ds.row_count);
    let sorted = View::build(&ds, ViewSpec { sort: vec![SortKey::desc(ds.columns[2].name.clone())], ..Default::default() })
        .wait()
        .expect("sort");
    let t = std::time::Instant::now();
    sorted.fetch_page(PageRequest { rows: 0..200, columns: 0..ds.columns.len() }).wait().expect("sorted page");
    eprintln!("sorted page in {:?}", t.elapsed());
}
