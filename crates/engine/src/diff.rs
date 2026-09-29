//! Comparing two datasets: schema, row counts and row-level differences.

use std::time::Instant;

use duckdb::Connection;

use crate::dataset::{Dataset, DatasetOrigin};
use crate::engine::{Job, Lane};
use crate::error::{Error, Result};
use crate::sql::ident;
use crate::types::ColumnInfo;

#[derive(Debug, Clone, Default)]
pub struct CompareOptions {
    /// Columns identifying a row. Without keys, rows are compared as whole records.
    pub keys: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct TypeChange {
    pub column: String,
    pub left: String,
    pub right: String,
}

#[derive(Debug, Clone)]
pub struct ColumnChanges {
    pub column: String,
    pub changed_rows: u64,
}

#[derive(Debug, Clone)]
pub struct Comparison {
    pub left_name: String,
    pub right_name: String,
    pub left_rows: u64,
    pub right_rows: u64,
    pub only_left_columns: Vec<ColumnInfo>,
    pub only_right_columns: Vec<ColumnInfo>,
    pub type_changes: Vec<TypeChange>,
    pub common_columns: Vec<String>,
    pub keys: Vec<String>,
    /// Rows in left but not right (by key, or as whole records).
    pub only_left: Dataset,
    pub only_right: Dataset,
    /// Rows whose key matches but other values differ (keyed comparisons only).
    pub changed: Option<Dataset>,
    pub column_changes: Vec<ColumnChanges>,
    /// Keys that appear more than once on either side (keyed comparisons only).
    pub duplicate_keys: u64,
    pub millis: u64,
}

impl Comparison {
    pub fn is_identical(&self) -> bool {
        self.only_left_columns.is_empty()
            && self.only_right_columns.is_empty()
            && self.type_changes.is_empty()
            && self.only_left.row_count == 0
            && self.only_right.row_count == 0
            && self.changed.as_ref().is_none_or(|c| c.row_count == 0)
    }
}

pub fn compare(left: &Dataset, right: &Dataset, options: CompareOptions) -> Job<Comparison> {
    let left = left.clone();
    let right = right.clone();
    let engine = left.engine.clone();
    engine.run(Lane::Task, move |conn| compare_now(conn, &left, &right, &options))
}

fn compare_now(
    conn: &Connection,
    left: &Dataset,
    right: &Dataset,
    options: &CompareOptions,
) -> Result<Comparison> {
    let started = Instant::now();
    left.prepare_remote(conn)?;
    right.prepare_remote(conn)?;
    let engine = &left.engine;
    let limit = engine.settings().result_limit_rows;

    let only_left_columns: Vec<ColumnInfo> = left
        .columns
        .iter()
        .filter(|c| right.column_index(&c.name).is_none())
        .cloned()
        .collect();
    let only_right_columns: Vec<ColumnInfo> = right
        .columns
        .iter()
        .filter(|c| left.column_index(&c.name).is_none())
        .cloned()
        .collect();
    let mut type_changes = Vec::new();
    let mut common = Vec::new();
    for column in &left.columns {
        if let Some(ix) = right.column_index(&column.name) {
            let other = &right.columns[ix];
            if other.sql_type != column.sql_type {
                type_changes.push(TypeChange {
                    column: column.name.clone(),
                    left: column.sql_type.clone(),
                    right: other.sql_type.clone(),
                });
            }
            common.push(column.name.clone());
        }
    }
    if common.is_empty() {
        return Err(Error::other("The two datasets have no columns in common"));
    }
    for key in &options.keys {
        if !common.contains(key) {
            return Err(Error::other(format!("Key column “{key}” must exist in both datasets")));
        }
    }
    let changed_type = |name: &str| type_changes.iter().any(|t| t.column == name);
    // Columns whose types differ are compared as text.
    let expr = |alias: &str, name: &str| -> String {
        if changed_type(name) {
            format!("CAST({alias}.{} AS VARCHAR)", ident(name))
        } else {
            format!("{alias}.{}", ident(name))
        }
    };
    let projection = |alias: &str| -> String {
        common
            .iter()
            .map(|c| format!("{} AS {}", expr(alias, c), ident(c)))
            .collect::<Vec<_>>()
            .join(", ")
    };
    let l = left.relation_name();
    let r = right.relation_name();
    let id = engine.next_id();
    let only_left_table = format!("pq_diff_l_{id}");
    let only_right_table = format!("pq_diff_r_{id}");
    let mut changed_table = None;
    let mut column_changes = Vec::new();
    let mut duplicate_keys = 0u64;

    if options.keys.is_empty() {
        conn.execute_batch(&format!(
            "CREATE TABLE {only_left_table} AS SELECT * FROM (SELECT {pl} FROM {l} a EXCEPT ALL SELECT {pr} FROM {r} b) LIMIT {limit};
             CREATE TABLE {only_right_table} AS SELECT * FROM (SELECT {pr} FROM {r} b EXCEPT ALL SELECT {pl} FROM {l} a) LIMIT {limit};",
            pl = projection("a"),
            pr = projection("b"),
        ))?;
    } else {
        let on = options
            .keys
            .iter()
            .map(|k| format!("{} IS NOT DISTINCT FROM {}", expr("a", k), expr("b", k)))
            .collect::<Vec<_>>()
            .join(" AND ");
        let keys_a = options
            .keys
            .iter()
            .map(|k| expr("a", k))
            .collect::<Vec<_>>()
            .join(", ");
        let keys_b = options
            .keys
            .iter()
            .map(|k| expr("b", k))
            .collect::<Vec<_>>()
            .join(", ");
        let dup: i64 = conn.query_row(
            &format!(
                "SELECT (SELECT count(*) FROM (SELECT {keys_a} FROM {l} a GROUP BY ALL HAVING count(*) > 1))
                      + (SELECT count(*) FROM (SELECT {keys_b} FROM {r} b GROUP BY ALL HAVING count(*) > 1))"
            ),
            [],
            |row| row.get(0),
        )?;
        duplicate_keys = dup.max(0) as u64;
        conn.execute_batch(&format!(
            "CREATE TABLE {only_left_table} AS SELECT a.* FROM {l} a ANTI JOIN {r} b ON {on} LIMIT {limit};
             CREATE TABLE {only_right_table} AS SELECT b.* FROM {r} b ANTI JOIN {l} a ON {on} LIMIT {limit};"
        ))?;
        let values: Vec<&String> = common.iter().filter(|c| !options.keys.contains(c)).collect();
        if !values.is_empty() {
            let differs: Vec<String> = values
                .iter()
                .map(|c| format!("{} IS DISTINCT FROM {}", expr("a", c), expr("b", c)))
                .collect();
            let changed_list = values
                .iter()
                .map(|c| {
                    format!(
                        "CASE WHEN {} IS DISTINCT FROM {} THEN {} END",
                        expr("a", c),
                        expr("b", c),
                        crate::sql::literal(c)
                    )
                })
                .collect::<Vec<_>>()
                .join(", ");
            let mut select = Vec::new();
            for key in &options.keys {
                select.push(format!("{} AS {}", expr("a", key), ident(key)));
            }
            select.push(format!(
                "list_filter([{changed_list}], x -> x IS NOT NULL) AS changed_columns"
            ));
            for c in &values {
                select.push(format!("{} AS {}", expr("a", c), ident(&format!("{c} (left)"))));
                select.push(format!("{} AS {}", expr("b", c), ident(&format!("{c} (right)"))));
            }
            let table = format!("pq_diff_c_{id}");
            conn.execute_batch(&format!(
                "CREATE TABLE {table} AS SELECT {} FROM {l} a JOIN {r} b ON {on} WHERE {} LIMIT {limit}",
                select.join(", "),
                differs.join(" OR ")
            ))?;
            let counts: Vec<String> = values
                .iter()
                .map(|c| format!("count(*) FILTER (WHERE list_contains(changed_columns, {}))", crate::sql::literal(c)))
                .collect();
            let mut stmt = conn.prepare(&format!("SELECT {} FROM {table}", counts.join(", ")))?;
            let mut rows = stmt.query([])?;
            if let Some(row) = rows.next()? {
                for (i, c) in values.iter().enumerate() {
                    let n: i64 = row.get(i)?;
                    if n > 0 {
                        column_changes.push(ColumnChanges {
                            column: (*c).clone(),
                            changed_rows: n as u64,
                        });
                    }
                }
            }
            changed_table = Some(table);
        }
    }

    let only_left = Dataset::from_table(
        engine,
        conn,
        only_left_table,
        format!("Only in {}", left.name),
        DatasetOrigin::Derived("comparison".into()),
        Vec::new(),
    )?;
    let only_right = Dataset::from_table(
        engine,
        conn,
        only_right_table,
        format!("Only in {}", right.name),
        DatasetOrigin::Derived("comparison".into()),
        Vec::new(),
    )?;
    let changed = match changed_table {
        Some(table) => Some(Dataset::from_table(
            engine,
            conn,
            table,
            "Changed rows".into(),
            DatasetOrigin::Derived("comparison".into()),
            Vec::new(),
        )?),
        None => None,
    };
    Ok(Comparison {
        left_name: left.name.clone(),
        right_name: right.name.clone(),
        left_rows: left.row_count,
        right_rows: right.row_count,
        only_left_columns,
        only_right_columns,
        type_changes,
        common_columns: common,
        keys: options.keys.clone(),
        only_left,
        only_right,
        changed,
        column_changes,
        duplicate_keys,
        millis: started.elapsed().as_millis() as u64,
    })
}
