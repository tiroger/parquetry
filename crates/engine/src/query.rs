//! Running SQL typed by the person.

use std::time::Instant;

use duckdb::Connection;

use crate::dataset::{Dataset, DatasetOrigin};
use crate::engine::{Engine, Job, Lane};
use crate::error::{Error, Result};
use crate::sql::ident;

/// Outcome of running SQL.
#[derive(Debug, Clone)]
pub struct QueryOutcome {
    /// Rows returned by the last statement, if it returned rows.
    pub result: Option<Dataset>,
    /// A short message for statements without rows ("Done", "3 statements run").
    pub message: Option<String>,
    /// The result was cut at the engine's result limit.
    pub truncated: bool,
    pub millis: u64,
}

/// A dataset exposed to SQL under a name.
#[derive(Clone)]
pub struct SqlTable {
    pub name: String,
    pub dataset: Dataset,
}

/// Run `sql` with `tables` available by name. Rows of the last statement are kept
/// (up to the engine's result limit) as a new dataset.
pub fn run_sql(engine: &Engine, sql: String, tables: Vec<SqlTable>) -> Job<QueryOutcome> {
    let engine_for_job = engine.clone();
    engine.run(Lane::Task, move |conn| run_now(&engine_for_job, conn, &sql, &tables))
}

fn run_now(engine: &Engine, conn: &Connection, sql: &str, tables: &[SqlTable]) -> Result<QueryOutcome> {
    let started = Instant::now();
    for table in tables {
        table.dataset.prepare_remote(conn)?;
        conn.execute_batch(&format!(
            "CREATE OR REPLACE TEMP VIEW {} AS SELECT * FROM {}",
            ident(&table.name),
            table.dataset.relation_name()
        ))?;
    }
    // Any S3 URLs typed directly into the query need credentials too.
    for url in s3_urls_in(sql) {
        engine.inner.s3.prepare_duckdb(engine, conn, &url)?;
    }

    let statements = split_statements(sql);
    if statements.is_empty() {
        return Err(Error::other("Type a query to run"));
    }
    let (last, before) = statements.split_last().unwrap();
    for statement in before {
        conn.execute_batch(statement)?;
    }

    let limit = engine.settings().result_limit_rows;
    let table = format!("pq_q_{}", engine.next_id());
    let create = format!("CREATE TABLE {table} AS SELECT * FROM ({last}) LIMIT {limit}");
    match conn.execute_batch(&create) {
        Ok(()) => {
            let dataset = Dataset::from_table(
                engine,
                conn,
                table,
                "Query result".into(),
                DatasetOrigin::Query(sql.to_string()),
                Vec::new(),
            )?;
            let truncated = dataset.row_count >= limit;
            Ok(QueryOutcome {
                result: Some(dataset),
                message: None,
                truncated,
                millis: started.elapsed().as_millis() as u64,
            })
        }
        Err(wrap_error) => {
            // Not something that can be wrapped in a subquery (SET, PRAGMA, CREATE, COPY...):
            // run it as-is. If that fails too, the original statement's error is the useful one.
            let wrap_error = Error::from(wrap_error);
            if wrap_error.is_cancelled() {
                return Err(wrap_error);
            }
            match conn.execute_batch(last) {
                Ok(()) => Ok(QueryOutcome {
                    result: None,
                    message: Some(if statements.len() > 1 {
                        format!("{} statements run", statements.len())
                    } else {
                        "Done".into()
                    }),
                    truncated: false,
                    millis: started.elapsed().as_millis() as u64,
                }),
                Err(direct_error) => Err(Error::from(direct_error)),
            }
        }
    }
}

/// Split SQL into statements on `;`, respecting quotes and comments.
pub fn split_statements(sql: &str) -> Vec<String> {
    let mut statements = Vec::new();
    let mut current = String::new();
    let chars: Vec<char> = sql.chars().collect();
    let mut i = 0;
    let mut quote: Option<char> = None;
    while i < chars.len() {
        let c = chars[i];
        match quote {
            Some(q) => {
                current.push(c);
                if c == q {
                    if i + 1 < chars.len() && chars[i + 1] == q {
                        current.push(q);
                        i += 1;
                    } else {
                        quote = None;
                    }
                }
            }
            None => {
                if c == '\'' || c == '"' {
                    quote = Some(c);
                    current.push(c);
                } else if c == '-' && i + 1 < chars.len() && chars[i + 1] == '-' {
                    while i < chars.len() && chars[i] != '\n' {
                        i += 1;
                    }
                    current.push('\n');
                } else if c == '/' && i + 1 < chars.len() && chars[i + 1] == '*' {
                    i += 2;
                    while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                        i += 1;
                    }
                    i += 1;
                    current.push(' ');
                } else if c == ';' {
                    let statement = current.trim().to_string();
                    if !statement.is_empty() {
                        statements.push(statement);
                    }
                    current.clear();
                } else {
                    current.push(c);
                }
            }
        }
        i += 1;
    }
    let statement = current.trim().to_string();
    if !statement.is_empty() {
        statements.push(statement);
    }
    statements
}

fn s3_urls_in(sql: &str) -> Vec<String> {
    let mut urls = Vec::new();
    for part in sql.split(['\'', '"']) {
        if part.starts_with("s3://") {
            urls.push(part.to_string());
        }
    }
    urls
}

/// A safe SQL table name derived from a file name: `sales-2024.parquet` → `sales_2024`.
pub fn table_name_for(name: &str) -> String {
    let stem = name.split('.').next().unwrap_or(name);
    let mut out: String = stem
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' })
        .collect();
    while out.contains("__") {
        out = out.replace("__", "_");
    }
    let out = out.trim_matches('_').to_string();
    if out.is_empty() || out.chars().next().unwrap().is_ascii_digit() {
        format!("t_{out}")
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits() {
        assert_eq!(
            split_statements("select 1; select ';' as x; -- c;\n select \"a;b\" /* ; */"),
            vec!["select 1", "select ';' as x", "select \"a;b\""]
        );
        assert_eq!(split_statements("  ;; "), Vec::<String>::new());
        assert_eq!(split_statements("select 'it''s;'"), vec!["select 'it''s;'"]);
    }

    #[test]
    fn names() {
        assert_eq!(table_name_for("sales-2024.parquet"), "sales_2024");
        assert_eq!(table_name_for("2024.parquet"), "t_2024");
        assert_eq!(table_name_for("My File (1).csv"), "my_file_1");
    }
}
