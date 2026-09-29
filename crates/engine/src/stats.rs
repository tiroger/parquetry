//! Column summaries: the histograms, top values and numbers shown in column headers.
//!
//! Summaries of large datasets are computed from a sample of evenly spaced row
//! ranges. For Parquet, row ranges map to row groups, so a sample reads a handful
//! of row groups instead of the whole file. Null counts, minimums and maximums come
//! from Parquet footers when possible, which makes them exact even when sampled.

use std::collections::HashMap;

use duckdb::Connection;

use crate::dataset::{Base, Dataset, FILENAME_COLUMN, SampleTable};
use crate::engine::{Job, Lane};
use crate::error::Result;
use crate::filter::qualified;
use crate::sql::{ident, literal, literal_list};
use crate::types::{ColumnInfo, ColumnKind};
use crate::view::View;

/// Number of histogram bins.
pub const HISTOGRAM_BINS: usize = 20;
/// Distinct values are counted exactly up to this many scanned rows.
const EXACT_DISTINCT_ROWS: u64 = 20_000_000;

/// Number of most frequent values kept.
pub const TOP_VALUES: usize = 10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatsMode {
    /// Sample large datasets.
    Auto,
    /// Scan everything.
    Exact,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HistogramBin {
    pub start: f64,
    pub end: f64,
    pub count: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TopValue {
    /// `None` is SQL NULL.
    pub value: Option<String>,
    pub count: u64,
}

/// Summary of one column of a view.
#[derive(Debug, Clone, PartialEq)]
pub struct ColumnSummary {
    pub column: String,
    pub kind: ColumnKind,
    /// Rows in the view.
    pub rows: u64,
    /// Rows the distribution was computed from (equals `rows` unless sampled).
    pub scanned_rows: u64,
    pub sampled: bool,
    /// Nulls among scanned rows.
    pub scanned_nulls: u64,
    /// Exact null count over all rows, when known (unsampled, or from Parquet footers).
    pub exact_nulls: Option<u64>,
    /// Distinct non-null values among scanned rows.
    pub distinct: Option<u64>,
    /// `distinct` was counted exactly (otherwise it's a HyperLogLog estimate).
    pub distinct_exact: bool,
    pub min: Option<String>,
    pub max: Option<String>,
    /// Min/max came from Parquet footers and cover all rows.
    pub min_max_exact: bool,
    pub mean: Option<f64>,
    pub std_dev: Option<f64>,
    /// 25th, 50th and 75th percentiles as display text.
    pub quantiles: Option<[String; 3]>,
    pub histogram: Vec<HistogramBin>,
    /// For temporal histograms: bin edges are microseconds since the epoch.
    pub histogram_is_time: bool,
    pub top_values: Vec<TopValue>,
    /// String length: min, mean, max.
    pub text_length: Option<(u64, f64, u64)>,
    pub millis: u64,
}

impl ColumnSummary {
    /// Fraction of rows that are null (exact when known, else from the sample).
    pub fn null_fraction(&self) -> f64 {
        if let Some(nulls) = self.exact_nulls {
            if self.rows == 0 {
                return 0.0;
            }
            return nulls as f64 / self.rows as f64;
        }
        if self.scanned_rows == 0 {
            0.0
        } else {
            self.scanned_nulls as f64 / self.scanned_rows as f64
        }
    }

    /// Whether every scanned non-null value is distinct.
    pub fn is_unique(&self) -> bool {
        let non_null = self.scanned_rows.saturating_sub(self.scanned_nulls);
        non_null > 1
            && self.top_values.iter().all(|v| v.value.is_none() || v.count == 1)
    }

    /// The best compact visualization for this column.
    pub fn preferred_chart(&self) -> ChartKind {
        if !self.histogram.is_empty() {
            return ChartKind::Histogram;
        }
        if self.top_values.is_empty() || self.is_unique() {
            return ChartKind::None;
        }
        // Bars only help when a few values account for a real share of rows;
        // otherwise (ids, names, hashes) they're noise and a count says more.
        let covered: u64 = self.top_values.iter().map(|v| v.count).sum();
        let few_values = self.distinct.is_some_and(|d| d <= 50);
        if few_values || covered as f64 >= self.scanned_rows as f64 * 0.25 {
            ChartKind::TopValues
        } else {
            ChartKind::None
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChartKind {
    Histogram,
    TopValues,
    None,
}

/// Summarize `columns` (indices into the dataset's columns) of `view`.
/// Results arrive per batch through the returned job, in the requested order.
pub fn summarize(view: &View, columns: Vec<usize>, mode: StatsMode) -> Job<Vec<ColumnSummary>> {
    let view = view.clone();
    let engine = view.dataset.engine.clone();
    engine.run(Lane::Background, move |conn| summarize_now(conn, &view, &columns, mode))
}

struct Relation {
    sql: String,
    scanned_rows: u64,
    sampled: bool,
}

fn summarize_now(
    conn: &Connection,
    view: &View,
    columns: &[usize],
    mode: StatsMode,
) -> Result<Vec<ColumnSummary>> {
    view.dataset.prepare_remote(conn)?;
    let relation = choose_relation(conn, view, mode)?;
    let selected: Vec<&ColumnInfo> = columns
        .iter()
        .filter_map(|&ix| view.dataset.columns.get(ix))
        .collect();
    if selected.is_empty() {
        return Ok(Vec::new());
    }

    let footer = if view.is_identity() && relation.sampled {
        footer_stats(conn, &view.dataset, &selected)
    } else {
        HashMap::new()
    };

    let mut out = Vec::with_capacity(selected.len());
    let base = basic_stats(conn, &relation, &selected)?;
    for (column, basic) in selected.iter().zip(base) {
        let started = std::time::Instant::now();
        let mut summary = ColumnSummary {
            column: column.name.clone(),
            kind: column.kind,
            rows: view.row_count,
            scanned_rows: relation.scanned_rows,
            sampled: relation.sampled,
            scanned_nulls: relation.scanned_rows.saturating_sub(basic.non_null),
            exact_nulls: None,
            distinct: basic.distinct,
            distinct_exact: relation.scanned_rows <= EXACT_DISTINCT_ROWS,
            min: basic.min,
            max: basic.max,
            min_max_exact: false,
            mean: basic.mean,
            std_dev: basic.std_dev,
            quantiles: basic.quantiles,
            histogram: Vec::new(),
            histogram_is_time: column.kind.is_temporal(),
            top_values: Vec::new(),
            text_length: basic.text_length,
            millis: 0,
        };
        if !relation.sampled {
            summary.exact_nulls = Some(summary.scanned_nulls);
        }
        if let Some(footer) = footer.get(&column.name) {
            if let Some(nulls) = footer.nulls {
                summary.exact_nulls = Some(nulls);
            }
            if footer.min.is_some() && footer.max.is_some() {
                summary.min = footer.min.clone();
                summary.max = footer.max.clone();
                summary.min_max_exact = true;
            }
        }

        let many_values = summary.distinct.unwrap_or(0) > 12;
        let histogram_worthy = (column.kind.is_numeric() || column.kind.is_temporal()) && many_values;
        if histogram_worthy
            && let (Some(lo), Some(hi)) = (basic.numeric_min, basic.numeric_max)
                && lo.is_finite() && hi.is_finite() && hi > lo {
                    summary.histogram = histogram(conn, &relation, column, lo, hi)?;
                }
        if summary.histogram.is_empty() && summary.scanned_rows > 0 {
            summary.top_values = top_values(conn, &relation, column)?;
        }
        summary.millis = started.elapsed().as_millis() as u64 + basic.millis;
        out.push(summary);
    }
    Ok(out)
}

/// The rows to scan: everything, or an even sample for large datasets.
fn choose_relation(conn: &Connection, view: &View, mode: StatsMode) -> Result<Relation> {
    let settings = view.dataset.engine.settings();
    let dataset = &view.dataset;
    let threshold = if dataset.is_remote() {
        settings.remote_sample_threshold_rows
    } else {
        settings.sample_threshold_rows
    };
    let exact = |view: &View| -> Result<Relation> {
        Ok(Relation {
            sql: view.unordered_source_sql()?,
            scanned_rows: view.row_count,
            sampled: false,
        })
    };

    if view.is_index() {
        // Filtered/sorted view. Re-evaluating the filter for every statistic would
        // rescan the source many times, so read the matching rows once instead.
        if view.is_small_index() {
            view.materialize_now(conn)?;
            return exact(view);
        }
        if mode == StatsMode::Auto
            && let Some((table, rows)) = view.index_sample(conn, settings.sample_rows)? {
                return Ok(Relation {
                    sql: format!("SELECT * FROM {table}"),
                    scanned_rows: rows,
                    sampled: rows < view.row_count,
                });
            }
        return exact(view);
    }

    let large = view.row_count > threshold && mode == StatsMode::Auto;
    if view.is_materialized() || !large {
        return exact(view);
    }
    let sample_table = ensure_sample(conn, dataset, settings.sample_rows)?;
    Ok(Relation {
        sql: format!("SELECT * FROM {}", sample_table.table),
        scanned_rows: sample_table.rows,
        sampled: true,
    })
}

/// Copy an even sample of the dataset into a local table, once per dataset. Every
/// statistic of every column then reads the sample instead of the source, which
/// matters most for remote files.
fn ensure_sample(conn: &Connection, dataset: &Dataset, target: u64) -> Result<SampleTable> {
    let mut guard = dataset.sample.lock();
    if let Some(sample) = guard.as_ref() {
        return Ok(sample.clone());
    }
    let table = format!("pq_smp_{}", dataset.engine.next_id());
    let select = sample_source_sql(dataset, target)?;
    conn.execute_batch(&format!("CREATE TABLE {table} AS {select}"))?;
    let rows: i64 = conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))?;
    let sample = SampleTable {
        table,
        rows: rows.max(0) as u64,
    };
    *guard = Some(sample.clone());
    Ok(sample)
}

/// SQL for an even sample of about `target` rows of the dataset's source.
fn sample_source_sql(dataset: &Dataset, target: u64) -> Result<String> {
    let total = dataset.row_count.max(1);
    match &dataset.base {
        Base::Parquet {
            meta,
            files_table,
            row_ids: true,
            ..
        } => {
            // Evenly spaced chunks, mapped onto files by cumulative row counts. Each
            // chunk costs a row group read, which is a download for remote files.
            let chunks = if dataset.is_remote() { 3u64 } else { 64u64 };
            let chunk_rows = (target / chunks).max(1);
            let step = total / chunks;
            let mut conditions = Vec::new();
            let mut offsets = Vec::with_capacity(dataset.files.len());
            let mut acc = 0u64;
            for file in &dataset.files {
                offsets.push(acc);
                acc += file.rows.unwrap_or(0);
            }
            for i in 0..chunks {
                let start = i * step;
                let end = (start + chunk_rows).min(total);
                if files_table.is_none() {
                    conditions.push(format!(
                        "s.file_row_number BETWEEN {start} AND {}",
                        end.saturating_sub(1)
                    ));
                    continue;
                }
                for (fix, file) in dataset.files.iter().enumerate() {
                    let file_start = offsets[fix];
                    let file_end = file_start + file.rows.unwrap_or(0);
                    if end <= file_start || start >= file_end {
                        continue;
                    }
                    let local_start = start.max(file_start) - file_start;
                    let local_end = end.min(file_end) - file_start;
                    conditions.push(format!(
                        "(s.{FILENAME_COLUMN} = {} AND s.file_row_number BETWEEN {local_start} AND {})",
                        literal(&file.path),
                        local_end.saturating_sub(1)
                    ));
                }
            }
            let columns: Vec<String> = dataset
                .columns
                .iter()
                .map(|c| qualified(Some("s"), &c.name))
                .collect();
            Ok(format!(
                "SELECT {} FROM {meta} s WHERE {}",
                columns.join(", "),
                conditions.join(" OR ")
            ))
        }
        Base::Table { table, .. } => Ok(format!(
            "SELECT * FROM {table} USING SAMPLE reservoir({target} ROWS) REPEATABLE (7)"
        )),
        Base::Relation { src } | Base::Parquet { src, row_ids: false, .. } => {
            Ok(format!("SELECT * FROM {src} LIMIT {target}"))
        }
    }
}

struct BasicStats {
    non_null: u64,
    distinct: Option<u64>,
    min: Option<String>,
    max: Option<String>,
    numeric_min: Option<f64>,
    numeric_max: Option<f64>,
    mean: Option<f64>,
    std_dev: Option<f64>,
    quantiles: Option<[String; 3]>,
    text_length: Option<(u64, f64, u64)>,
    millis: u64,
}

/// Expression mapping a column to a DOUBLE axis for histograms.
fn axis_expr(column: &ColumnInfo) -> Option<String> {
    let c = ident(&column.name);
    match column.kind {
        ColumnKind::Integer | ColumnKind::Float | ColumnKind::Decimal => Some(format!("CAST({c} AS DOUBLE)")),
        ColumnKind::Date | ColumnKind::Timestamp => Some(format!("CAST(epoch_us(CAST({c} AS TIMESTAMP)) AS DOUBLE)")),
        _ => None,
    }
}

/// One scan for the simple aggregates of several columns.
fn basic_stats(conn: &Connection, relation: &Relation, columns: &[&ColumnInfo]) -> Result<Vec<BasicStats>> {
    let started = std::time::Instant::now();
    let mut exprs = Vec::new();
    for column in columns {
        let c = ident(&column.name);
        exprs.push(format!("count({c})"));
        // DuckDB's HyperLogLog overestimates badly at high cardinality (70M for
        // 50M unique values), and exact counts are cheap at these sizes.
        let func = if relation.scanned_rows <= EXACT_DISTINCT_ROWS {
            "count(DISTINCT {})"
        } else {
            "approx_count_distinct({})"
        };
        let target = if column.kind.is_nested() {
            format!("CAST({c} AS VARCHAR)")
        } else {
            c.clone()
        };
        let distinct = func.replace("{}", &target);
        exprs.push(distinct);
        if column.kind.is_nested() || matches!(column.kind, ColumnKind::Binary) {
            exprs.push("NULL::VARCHAR".into());
            exprs.push("NULL::VARCHAR".into());
        } else {
            exprs.push(format!("CAST(min({c}) AS VARCHAR)"));
            exprs.push(format!("CAST(max({c}) AS VARCHAR)"));
        }
        match axis_expr(column) {
            Some(axis) => {
                let finite = if column.kind == ColumnKind::Float {
                    format!(" FILTER (WHERE isfinite({c}))")
                } else {
                    String::new()
                };
                exprs.push(format!("min({axis}){finite}"));
                exprs.push(format!("max({axis}){finite}"));
            }
            None => {
                exprs.push("NULL::DOUBLE".into());
                exprs.push("NULL::DOUBLE".into());
            }
        }
        if column.kind.is_numeric() {
            let finite = if column.kind == ColumnKind::Float {
                format!(" FILTER (WHERE isfinite({c}))")
            } else {
                String::new()
            };
            exprs.push(format!("avg(CAST({c} AS DOUBLE)){finite}"));
            exprs.push(format!("stddev_samp(CAST({c} AS DOUBLE)){finite}"));
            exprs.push(format!(
                "CAST(approx_quantile(CAST({c} AS DOUBLE), [0.25, 0.5, 0.75]){finite} AS VARCHAR[])"
            ));
        } else if column.kind.is_temporal() {
            exprs.push("NULL::DOUBLE".into());
            exprs.push("NULL::DOUBLE".into());
            exprs.push(format!(
                "CAST(quantile_disc({c}, [0.25, 0.5, 0.75]) AS VARCHAR[])"
            ));
        } else {
            exprs.push("NULL::DOUBLE".into());
            exprs.push("NULL::DOUBLE".into());
            exprs.push("NULL::VARCHAR[]".into());
        }
        if column.kind == ColumnKind::String {
            exprs.push(format!("min(length({c}))"));
            exprs.push(format!("avg(length({c}))"));
            exprs.push(format!("max(length({c}))"));
        } else {
            exprs.push("NULL::BIGINT".into());
            exprs.push("NULL::DOUBLE".into());
            exprs.push("NULL::BIGINT".into());
        }
    }
    const PER_COLUMN: usize = 12;
    let sql = format!("SELECT {} FROM ({}) t", exprs.join(", "), relation.sql);
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query([])?;
    let row = rows
        .next()?
        .ok_or_else(|| crate::Error::other("No result from summary query"))?;
    let mut out = Vec::with_capacity(columns.len());
    for (i, column) in columns.iter().enumerate() {
        let b = i * PER_COLUMN;
        let non_null: i64 = row.get(b)?;
        let distinct: Option<i64> = row.get(b + 1)?;
        let min: Option<String> = row.get(b + 2)?;
        let max: Option<String> = row.get(b + 3)?;
        let numeric_min: Option<f64> = row.get(b + 4)?;
        let numeric_max: Option<f64> = row.get(b + 5)?;
        let mean: Option<f64> = row.get(b + 6)?;
        let std_dev: Option<f64> = row.get(b + 7)?;
        let quantiles = list_of_strings(row.get_ref(b + 8)?);
        let len_min: Option<i64> = row.get(b + 9)?;
        let len_avg: Option<f64> = row.get(b + 10)?;
        let len_max: Option<i64> = row.get(b + 11)?;
        let quantiles = quantiles.and_then(|q| {
            if q.len() == 3 {
                let format = |s: &String| -> String {
                    if column.kind.is_numeric() {
                        s.parse::<f64>().map(format_number).unwrap_or_else(|_| s.clone())
                    } else {
                        s.clone()
                    }
                };
                Some([format(&q[0]), format(&q[1]), format(&q[2])])
            } else {
                None
            }
        });
        out.push(BasicStats {
            non_null: non_null.max(0) as u64,
            distinct: distinct.map(|d| (d.max(0) as u64).min(non_null.max(0) as u64)),
            min,
            max,
            numeric_min,
            numeric_max,
            mean: mean.filter(|m| m.is_finite()),
            std_dev: std_dev.filter(|s| s.is_finite()),
            quantiles,
            text_length: match (len_min, len_avg, len_max) {
                (Some(a), Some(b), Some(c)) => Some((a.max(0) as u64, b, c.max(0) as u64)),
                _ => None,
            },
            millis: started.elapsed().as_millis() as u64 / columns.len().max(1) as u64,
        });
    }
    Ok(out)
}

fn list_of_strings(value: duckdb::types::ValueRef<'_>) -> Option<Vec<String>> {
    use duckdb::types::Value;
    match value.to_owned() {
        Value::List(items) => Some(
            items
                .into_iter()
                .map(|v| match v {
                    Value::Text(s) => s,
                    Value::Null => String::new(),
                    other => format!("{other:?}"),
                })
                .collect(),
        ),
        _ => None,
    }
}

fn histogram(
    conn: &Connection,
    relation: &Relation,
    column: &ColumnInfo,
    lo: f64,
    hi: f64,
) -> Result<Vec<HistogramBin>> {
    let Some(axis) = axis_expr(column) else {
        return Ok(Vec::new());
    };
    let bins = HISTOGRAM_BINS as i64;
    let width = (hi - lo) / bins as f64;
    let sql = format!(
        "SELECT least(greatest(floor((x - {lo:?}) / {width:?}), 0), {max_bin})::BIGINT AS b, count(*) AS n
         FROM (SELECT {axis} AS x FROM ({rel}) t) WHERE x IS NOT NULL AND isfinite(x) GROUP BY b ORDER BY b",
        max_bin = bins - 1,
        rel = relation.sql
    );
    let mut counts = vec![0u64; HISTOGRAM_BINS];
    let mut stmt = conn.prepare(&sql)?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let bin: i64 = row.get(0)?;
        let count: i64 = row.get(1)?;
        if (0..bins).contains(&bin) {
            counts[bin as usize] = count.max(0) as u64;
        }
    }
    Ok(counts
        .into_iter()
        .enumerate()
        .map(|(i, count)| HistogramBin {
            start: lo + width * i as f64,
            end: lo + width * (i + 1) as f64,
            count,
        })
        .collect())
}

fn top_values(conn: &Connection, relation: &Relation, column: &ColumnInfo) -> Result<Vec<TopValue>> {
    let c = ident(&column.name);
    let sql = format!(
        "SELECT CAST({c} AS VARCHAR) AS v, count(*) AS n FROM ({}) t GROUP BY v ORDER BY n DESC, v NULLS LAST LIMIT {TOP_VALUES}",
        relation.sql
    );
    let mut stmt = conn.prepare(&sql)?;
    let values = stmt
        .query_map([], |row| {
            Ok(TopValue {
                value: row.get::<_, Option<String>>(0)?,
                count: row.get::<_, i64>(1)?.max(0) as u64,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(values)
}

struct FooterStats {
    nulls: Option<u64>,
    min: Option<String>,
    max: Option<String>,
}

/// Exact null counts and min/max from Parquet footers (no data read).
fn footer_stats(conn: &Connection, dataset: &Dataset, columns: &[&ColumnInfo]) -> HashMap<String, FooterStats> {
    let mut out = HashMap::new();
    if !dataset.is_parquet() || dataset.files.is_empty() || dataset.files.len() > 500 {
        return out;
    }
    let files = literal_list(&dataset.parquet_files());
    for column in columns {
        if column.kind.is_nested() || matches!(column.kind, ColumnKind::Binary | ColumnKind::Other) {
            continue;
        }
        let min_max = if column.kind == ColumnKind::String {
            // String statistics may be truncated; trust them only when marked exact.
            "CASE WHEN bool_and(coalesce(min_is_exact, false)) THEN min(stats_min_value) END,
             CASE WHEN bool_and(coalesce(max_is_exact, false)) THEN max(stats_max_value) END"
                .to_string()
        } else {
            format!(
                "CAST(min(TRY_CAST(stats_min_value AS {t})) AS VARCHAR), CAST(max(TRY_CAST(stats_max_value AS {t})) AS VARCHAR)",
                t = column.sql_type
            )
        };
        let sql = format!(
            "SELECT CASE WHEN bool_and(stats_null_count IS NOT NULL) THEN sum(stats_null_count) END,
                    CASE WHEN bool_and(stats_min_value IS NOT NULL AND stats_max_value IS NOT NULL) THEN 1 END,
                    {min_max}
             FROM parquet_metadata({files}) WHERE path_in_schema = {}",
            literal(&column.name)
        );
        let result = conn.query_row(&sql, [], |row| {
            let nulls: Option<i128> = row.get::<_, Option<i128>>(0).ok().flatten();
            let has_min_max: Option<i32> = row.get(1)?;
            let min: Option<String> = row.get(2)?;
            let max: Option<String> = row.get(3)?;
            Ok(FooterStats {
                nulls: nulls.map(|n| n.max(0) as u64),
                min: has_min_max.and(min),
                max: has_min_max.and(max),
            })
        });
        if let Ok(stats) = result {
            out.insert(column.name.clone(), stats);
        }
    }
    out
}

/// Compact number text: integers without decimals, others with up to 4 significant decimals.
pub fn format_number(value: f64) -> String {
    if !value.is_finite() {
        return value.to_string();
    }
    let abs = value.abs();
    if abs >= 1e15 || (abs > 0.0 && abs < 1e-4) {
        return format!("{value:.3e}");
    }
    if value.fract() == 0.0 {
        return format!("{}", value as i64);
    }
    let decimals = if abs >= 1000.0 {
        1
    } else if abs >= 1.0 {
        3
    } else {
        4
    };
    let text = format!("{value:.decimals$}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    text.to_string()
}

/// Format microseconds since the epoch as `YYYY-MM-DD` or `YYYY-MM-DD HH:MM:SS`.
pub fn format_epoch_micros(micros: f64, date_only: bool) -> String {
    let total_seconds = (micros / 1_000_000.0).floor() as i64;
    let days = total_seconds.div_euclid(86_400);
    let seconds = total_seconds.rem_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    if date_only {
        format!("{y:04}-{m:02}-{d:02}")
    } else {
        format!(
            "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}",
            seconds / 3600,
            (seconds / 60) % 60,
            seconds % 60
        )
    }
}

/// Howard Hinnant's days-to-civil algorithm.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers() {
        assert_eq!(format_number(3.0), "3");
        assert_eq!(format_number(1.23456), "1.235");
        assert_eq!(format_number(12345.678), "12345.7");
        assert_eq!(format_number(0.000012), "1.200e-5");
        assert_eq!(format_number(-0.5), "-0.5");
    }

    #[test]
    fn epochs() {
        assert_eq!(format_epoch_micros(0.0, true), "1970-01-01");
        assert_eq!(
            format_epoch_micros(1_577_836_800_000_000.0 + 3_661_000_000.0, false),
            "2020-01-01 01:01:01"
        );
        assert_eq!(format_epoch_micros(-86_400_000_000.0, true), "1969-12-31");
    }
}
