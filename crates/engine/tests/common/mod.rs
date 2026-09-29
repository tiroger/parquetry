//! Shared fixtures: small, deliberately awkward datasets written with DuckDB.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

use parquetry_engine::{Engine, EnginePaths, EngineSettings};

pub struct Fixture {
    pub dir: tempfile::TempDir,
    pub engine: Engine,
}

impl Fixture {
    pub fn new() -> Self {
        Self::with_settings(EngineSettings::default())
    }

    pub fn with_settings(settings: EngineSettings) -> Self {
        let dir = tempfile::tempdir().expect("tempdir");
        let engine = Engine::new(EnginePaths::in_dir(dir.path()), settings).expect("engine");
        Self { dir, engine }
    }

    pub fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    pub fn path_str(&self, name: &str) -> String {
        self.path(name).to_string_lossy().into_owned()
    }

    /// Run SQL on a private connection (for writing fixtures and checking answers).
    pub fn duck(&self) -> duckdb::Connection {
        duckdb::Connection::open_in_memory().expect("duckdb")
    }

    pub fn write(&self, sql_select: &str, name: &str, options: &str) -> String {
        let path = self.path_str(name);
        if let Some(parent) = Path::new(&path).parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let format_opts = if options.is_empty() {
            "FORMAT parquet".to_string()
        } else {
            options.to_string()
        };
        self.duck()
            .execute_batch(&format!(
                "COPY ({sql_select}) TO '{}' ({format_opts})",
                path.replace('\'', "''")
            ))
            .unwrap_or_else(|e| panic!("fixture {name}: {e}"));
        path
    }

    /// Every type we care about, with nulls, unicode, control characters and edge values.
    pub fn types_file(&self) -> String {
        self.write(
            r#"SELECT * FROM (VALUES
              (1::BIGINT, 1::TINYINT, 1.5::DOUBLE, 1.25::FLOAT, 12.34::DECIMAL(10,2), true, 'hello',
               DATE '2024-01-02', TIMESTAMP '2024-01-02 03:04:05.123456', TIMESTAMPTZ '2024-01-02 03:04:05+00',
               TIME '03:04:05', INTERVAL 3 DAY, '\xAA\xBB'::BLOB, '6f1f5c3e-3f3e-4a9f-9f0e-1e2d3c4b5a69'::UUID,
               [1, 2, 3], {'a': 1, 'b': 'x'}, MAP {'k': 1}, 18446744073709551615::UBIGINT, 170141183460469231731687303715884105727::HUGEINT),
              (2, -128, 'NaN'::DOUBLE, 'inf'::FLOAT, -99999999.99, false, 'multi
line	tab',
               DATE '1900-02-28', TIMESTAMP '1970-01-01 00:00:00', TIMESTAMPTZ '2000-06-01 12:00:00+05',
               TIME '23:59:59', INTERVAL 1 MONTH, ''::BLOB, NULL, [], {'a': NULL, 'b': NULL}, MAP {}, 0, -1),
              (3, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL, NULL),
              (4, 127, -0.0, '-inf'::FLOAT, 0, true, '😀 ünïcödé "quoted" ''single''',
               DATE '9999-12-31', TIMESTAMP '2262-04-11 23:47:16', TIMESTAMPTZ '1969-12-31 23:59:59+00',
               TIME '00:00:00', INTERVAL 0 SECOND, '\x00'::BLOB, '00000000-0000-0000-0000-000000000000'::UUID,
               [NULL], {'a': 2, 'b': ''}, MAP {'a': NULL}, 1, 1)
            ) t(id, tiny, dbl, flt, dec, flag, txt, d, ts, tstz, tm, iv, bin, uid, lst, st, mp, ubig, huge)"#,
            "types.parquet",
            "",
        )
    }

    /// `rows` rows in row groups of `group` rows: id, amount, color, ts, maybe_null, txt.
    pub fn big_file(&self, name: &str, rows: u64, group: u64) -> String {
        self.write(
            &format!(
                "SELECT i AS id,
                        (hash(i * 7) % 100000) / 100.0 AS amount,
                        ['red', 'green', 'blue', 'yellow', 'purple'][1 + (hash(i * 3) % 5)::INT] AS color,
                        TIMESTAMP '2020-01-01' + INTERVAL (hash(i * 11) % 100000000) SECOND AS ts,
                        CASE WHEN i % 10 = 0 THEN NULL ELSE (i % 1000)::INTEGER END AS maybe_null,
                        'row ' || i AS txt
                 FROM range({rows}) t(i)"
            ),
            name,
            &format!("FORMAT parquet, ROW_GROUP_SIZE {group}"),
        )
    }
}
