//! Generated code returns the same rows as the app: every case is run in a real
//! Python (through `uv`) and compared with the engine's view.
//!
//! Skipped when `uv` isn't installed, unless PARQUETRY_REQUIRE_PYTHON_TESTS=1 (CI).

mod common;

use std::process::Command;

use common::Fixture;
use parquetry_engine::*;

fn uv_available() -> bool {
    Command::new("uv").arg("--version").output().is_ok_and(|o| o.status.success())
}

struct Case {
    name: String,
    dataset: Dataset,
    spec: ViewSpec,
    flavor: CodeFlavor,
    columns: Vec<String>,
    /// Expect native Polars (true) or DuckDB (false); `None` for other flavors.
    native: Option<bool>,
}

/// The first rows' `id`s and the row count, from the engine.
fn expected(dataset: &Dataset, spec: &ViewSpec) -> (u64, Vec<i64>) {
    let view = View::build(dataset, spec.clone()).wait().unwrap();
    let id = dataset.columns.iter().position(|c| c.name == "id").unwrap();
    let rows = view.fetch_values(0..20, vec![id], 20).wait().unwrap();
    let ids = rows.iter().map(|r| r[0].as_deref().unwrap().parse().unwrap()).collect();
    (view.row_count, ids)
}

fn run_cases(fx: &Fixture, cases: &[Case]) {
    if !uv_available() {
        assert!(
            std::env::var("PARQUETRY_REQUIRE_PYTHON_TESTS").is_err(),
            "uv is required for the generated-code tests"
        );
        eprintln!("skipping generated-code tests: uv not installed");
        return;
    }
    let mut script = String::from("import json\n\nresults = {}\n\n");
    for (ix, case) in cases.iter().enumerate() {
        let options = CodeOptions { columns: case.columns.clone(), ..Default::default() };
        let code = view_code(&case.dataset, &case.spec, case.flavor, &options)
            .unwrap_or_else(|e| panic!("{}: {e}", case.name));
        if let Some(native) = case.native {
            assert_eq!(code.contains("pl.scan_"), native, "{}: native Polars?\n{code}", case.name);
        }
        let body: String = match case.flavor {
            CodeFlavor::Sql => format!(
                "import duckdb\nrel = duckdb.sql({})\nrows = rel.fetchall()\nix = rel.columns.index(\"id\")\nreturn len(rows), [r[ix] for r in rows[:20]], rel.columns\n",
                serde_json::to_string(code.trim_end().trim_end_matches(';')).unwrap()
            ),
            CodeFlavor::Polars => format!("{code}\nreturn df.height, df[\"id\"].head(20).to_list(), df.columns\n"),
            CodeFlavor::Pandas => format!("{code}\nreturn len(df), [int(x) for x in df[\"id\"].head(20)], list(df.columns)\n"),
        };
        script.push_str(&format!("def case_{ix}():\n"));
        for line in body.lines() {
            script.push_str("    ");
            script.push_str(line);
            script.push('\n');
        }
        script.push_str(&format!(
            "\ntry:\n    n, ids, cols = case_{ix}()\n    results[{ix}] = {{\"rows\": n, \"ids\": ids, \"columns\": cols}}\nexcept Exception as e:\n    results[{ix}] = {{\"error\": repr(e)}}\n\n"
        ));
    }
    script.push_str("print(json.dumps(results))\n");
    let path = fx.path("generated_cases.py");
    std::fs::write(&path, &script).unwrap();
    let output = Command::new("uv")
        .args(["run", "--quiet", "--no-project", "--python", "3.12"])
        .args(["--with", "duckdb", "--with", "polars", "--with", "pandas", "--with", "pyarrow", "--with", "numpy"])
        .arg("python")
        .arg(&path)
        .output()
        .expect("run uv");
    assert!(output.status.success(), "python failed:\n{}", String::from_utf8_lossy(&output.stderr));
    let results: serde_json::Value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&output.stdout)));
    let native = cases.iter().filter(|c| c.native == Some(true)).count();
    eprintln!("checked {} generated-code cases in Python ({native} as native Polars)", cases.len());
    for (ix, case) in cases.iter().enumerate() {
        let got = &results[ix.to_string()];
        assert!(got.get("error").is_none(), "{} ({:?}): {}\n{}", case.name, case.flavor, got["error"], script_case(&script, ix));
        let (rows, ids) = expected(&case.dataset, &case.spec);
        assert_eq!(got["rows"].as_u64(), Some(rows), "{} ({:?}) row count", case.name, case.flavor);
        let got_ids: Vec<i64> = got["ids"].as_array().unwrap().iter().map(|v| v.as_i64().unwrap()).collect();
        assert_eq!(got_ids, ids, "{} ({:?}) first rows", case.name, case.flavor);
        if !case.columns.is_empty() {
            let cols: Vec<String> = got["columns"].as_array().unwrap().iter().map(|v| v.as_str().unwrap().to_string()).collect();
            assert_eq!(cols, case.columns, "{} ({:?}) columns", case.name, case.flavor);
        }
    }
}

fn script_case(script: &str, ix: usize) -> String {
    let start = script.find(&format!("def case_{ix}():")).unwrap_or(0);
    script[start..].lines().take(30).collect::<Vec<_>>().join("\n")
}

fn open(fx: &Fixture, location: &str) -> Dataset {
    Dataset::open(&fx.engine, SourceSpec::new(location)).wait().unwrap()
}

#[test]
fn generated_code_returns_the_views_rows() {
    let fx = Fixture::new();
    let select = "SELECT i AS id,
                CASE WHEN i % 13 = 0 THEN NULL ELSE ['eu', 'us', 'apac', 'o''hara'][1 + i % 4] END AS region,
                round((i * 7919 % 10000) / 7.0, 3) AS amount,
                CAST((i % 500) / 4.0 AS DECIMAL(9, 2)) AS price,
                DATE '2023-01-01' + CAST(i % 600 AS INTEGER) AS day,
                TIMESTAMP '2024-01-01 00:00:00' + to_microseconds(CAST(i AS BIGINT) * 61_000_000) AS ts,
                i % 3 = 0 AS flag,
                CASE WHEN i % 5 = 0 THEN '' ELSE 'note ' || i END AS note
         FROM range(5000) r(i)";
    let parquet = fx.write(select, "sales.parquet", "");
    let csv = fx.write(select, "sales.csv", "FORMAT csv, HEADER");
    // A partitioned folder: year=… becomes a column.
    let folder = fx.path_str("parts");
    for year in [2023, 2024] {
        fx.write(&format!("SELECT * FROM ({select}) WHERE year(day) = {year}"), &format!("parts/year={year}/data.parquet"), "");
    }
    let sales = open(&fx, &parquet);
    let sales_csv = open(&fx, &csv);
    let parts = open(&fx, &folder);

    let f = Filter::new;
    let views: Vec<(&str, ViewSpec, Option<bool>)> = vec![
        ("everything", ViewSpec::default(), Some(true)),
        ("equals + sort", ViewSpec { filters: vec![f("region", FilterOp::Equals, "eu")], sort: vec![SortKey::desc("amount"), SortKey::asc("id")], ..Default::default() }, Some(true)),
        ("quote in value", ViewSpec { filters: vec![f("region", FilterOp::Equals, "o'hara")], ..Default::default() }, Some(true)),
        ("not equals keeps nulls", ViewSpec { filters: vec![f("region", FilterOp::NotEquals, "us")], ..Default::default() }, Some(true)),
        ("in / not in", ViewSpec { filters: vec![f("region", FilterOp::In, "eu, apac"), f("id", FilterOp::NotIn, "4, 8, 12")], ..Default::default() }, Some(true)),
        ("numbers", ViewSpec { filters: vec![f("amount", FilterOp::GreaterOrEqual, "100.5"), f("id", FilterOp::Less, "4000")], sort: vec![SortKey::asc("amount"), SortKey::asc("id")], ..Default::default() }, Some(true)),
        ("between", ViewSpec::default(), Some(true)),
        ("dates", ViewSpec { filters: vec![f("day", FilterOp::Greater, "2024-03-01")], ..Default::default() }, Some(true)),
        ("timestamps", ViewSpec { filters: vec![f("ts", FilterOp::Less, "2024-01-02 12:00:00")], ..Default::default() }, Some(true)),
        ("text", ViewSpec { filters: vec![f("note", FilterOp::Contains, "NOTE 1"), f("note", FilterOp::NotContains, "9")], ..Default::default() }, Some(true)),
        ("starts/ends/empty", ViewSpec { filters: vec![f("note", FilterOp::StartsWith, "note 2"), f("note", FilterOp::IsNotEmpty, "")], ..Default::default() }, Some(true)),
        ("booleans + nulls", ViewSpec { filters: vec![f("flag", FilterOp::IsTrue, ""), f("region", FilterOp::IsNull, "")], ..Default::default() }, Some(true)),
        ("decimal goes through DuckDB", ViewSpec { filters: vec![f("price", FilterOp::Greater, "60.25")], ..Default::default() }, Some(false)),
        ("regex goes through DuckDB", ViewSpec { filters: vec![f("note", FilterOp::Matches, "^note 1[0-9]$")], ..Default::default() }, Some(false)),
        ("search goes through DuckDB", ViewSpec { search: "APAC".into(), sort: vec![SortKey::asc("id")], ..Default::default() }, Some(false)),
        ("custom WHERE goes through DuckDB", ViewSpec { where_sql: "id % 7 = 0 AND amount > 50".into(), ..Default::default() }, Some(false)),
    ];
    let mut cases = Vec::new();
    for (name, mut spec, native) in views {
        if name == "between" {
            spec.filters = vec![Filter::between("id", "100", "250")];
        }
        for flavor in [CodeFlavor::Sql, CodeFlavor::Polars, CodeFlavor::Pandas] {
            cases.push(Case {
                name: name.into(),
                dataset: sales.clone(),
                spec: spec.clone(),
                flavor,
                columns: Vec::new(),
                native: (flavor == CodeFlavor::Polars).then_some(native).flatten(),
            });
        }
    }
    // A subset of columns, CSV, and a partitioned folder.
    let some = vec!["id".to_string(), "amount".to_string()];
    let eu = ViewSpec { filters: vec![Filter::new("region", FilterOp::Equals, "eu")], sort: vec![SortKey::asc("id")], ..Default::default() };
    for flavor in [CodeFlavor::Sql, CodeFlavor::Polars, CodeFlavor::Pandas] {
        cases.push(Case { name: "columns".into(), dataset: sales.clone(), spec: eu.clone(), flavor, columns: some.clone(), native: None });
        cases.push(Case { name: "csv".into(), dataset: sales_csv.clone(), spec: eu.clone(), flavor, columns: Vec::new(), native: None });
        let by_year = ViewSpec {
            filters: vec![Filter::new("year", FilterOp::Equals, "2024"), Filter::new("region", FilterOp::Equals, "us")],
            sort: vec![SortKey::asc("id")],
            ..Default::default()
        };
        cases.push(Case { name: "hive folder".into(), dataset: parts.clone(), spec: by_year, flavor, columns: Vec::new(), native: None });
    }
    run_cases(&fx, &cases);
}

#[test]
fn query_results_have_no_code() {
    let fx = Fixture::new();
    let outcome = run_sql(&fx.engine, "SELECT 1 AS id".into(), Vec::new()).wait().unwrap();
    let dataset = outcome.result.expect("rows");
    let err = view_code(&dataset, &ViewSpec::default(), CodeFlavor::Polars, &CodeOptions::default()).unwrap_err();
    assert!(err.to_string().contains("opened from a file"), "{err}");
}
