//! A view is a dataset seen through filters, search and sort, addressable by row position.
//!
//! Reading row *n* of a view must be fast no matter how large the data is:
//!
//! - **Identity** (no filter/sort): `LIMIT/OFFSET` on the source. DuckDB skips whole
//!   Parquet row groups using footer row counts, so any page costs ~50 ms.
//! - **Index**: the matching rows' identities (file, row number) are written once, in
//!   view order, to a narrow DuckDB table. Building it reads only the filter and sort
//!   columns. A page then joins a slice of the index back to the source, reading only
//!   the row groups that contain those rows.
//! - **Materialized**: small results are copied into DuckDB entirely, so scrolling never
//!   touches the source again. Index views are upgraded in the background when small.

use std::ops::Range;
use std::sync::Arc;
use std::time::Instant;

use arrow::array::{Array, AsArray};
use duckdb::Connection;
use parking_lot::RwLock;

use crate::dataset::{Base, Dataset, FILENAME_COLUMN};
use crate::engine::{Job, Lane};
use crate::error::{Error, Result};
use crate::filter::{ViewSpec, qualified};
use crate::sql::literal;
use crate::types::ColumnInfo;

/// Longest cell text returned for display; the full value is fetched on demand.
pub const DISPLAY_TEXT_LIMIT: usize = 600;

#[derive(Debug, Clone)]
enum Access {
    Identity,
    Index { table: String },
    Materialized { table: String },
}

pub struct ViewInner {
    pub id: u64,
    pub dataset: Dataset,
    pub spec: ViewSpec,
    pub row_count: u64,
    /// The result was cut at the engine's result limit (non-Parquet relations only).
    pub truncated: bool,
    pub build_millis: u64,
    access: RwLock<Access>,
    /// Tables replaced by a newer access path; dropped with the view because an
    /// in-flight page fetch may still be reading them.
    retired: parking_lot::Mutex<Vec<String>>,
    /// Serializes materialization between the background job and statistics.
    materialize_lock: parking_lot::Mutex<()>,
    /// A random sample of this view's rows, for statistics of large filtered views.
    stats_sample: parking_lot::Mutex<Option<(String, u64)>>,
}

/// A dataset through a [`ViewSpec`]. Cheap to clone.
#[derive(Clone)]
pub struct View(Arc<ViewInner>);

impl std::ops::Deref for View {
    type Target = ViewInner;
    fn deref(&self) -> &ViewInner {
        &self.0
    }
}

impl std::fmt::Debug for View {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("View")
            .field("dataset", &self.dataset.name)
            .field("rows", &self.row_count)
            .field("spec", &self.spec)
            .finish()
    }
}

impl Drop for ViewInner {
    fn drop(&mut self) {
        let mut tables = std::mem::take(&mut *self.retired.lock());
        if let Some((table, _)) = self.stats_sample.lock().take() {
            tables.push(table);
        }
        match &*self.access.read() {
            Access::Identity => {}
            Access::Index { table } | Access::Materialized { table } => tables.push(table.clone()),
        }
        for table in tables {
            self.dataset
                .engine
                .run_detached(format!("DROP TABLE IF EXISTS {table}"));
        }
    }
}

/// Which rows and columns to fetch.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PageRequest {
    pub rows: Range<u64>,
    pub columns: Range<usize>,
}

/// Display text for a block of cells.
#[derive(Debug, Clone)]
pub struct Page {
    pub rows: Range<u64>,
    pub columns: Range<usize>,
    /// Row-major; `None` is SQL NULL.
    cells: Vec<Option<Arc<str>>>,
}

impl Page {
    pub fn row_count(&self) -> usize {
        (self.rows.end - self.rows.start) as usize
    }

    /// The cell at absolute row/column, if this page covers it.
    pub fn cell(&self, row: u64, column: usize) -> Option<&Option<Arc<str>>> {
        if !self.rows.contains(&row) || !self.columns.contains(&column) {
            return None;
        }
        let width = self.columns.len();
        let ix = (row - self.rows.start) as usize * width + (column - self.columns.start);
        self.cells.get(ix)
    }

    pub fn approx_bytes(&self) -> usize {
        self.cells
            .iter()
            .map(|c| c.as_ref().map_or(8, |s| s.len() + 24))
            .sum()
    }
}

impl View {
    /// The dataset as-is.
    pub fn identity(dataset: &Dataset) -> View {
        View(Arc::new(ViewInner {
            id: dataset.engine.next_id(),
            dataset: dataset.clone(),
            spec: ViewSpec::default(),
            row_count: dataset.row_count,
            truncated: false,
            build_millis: 0,
            access: RwLock::new(Access::Identity),
            retired: Default::default(),
            materialize_lock: Default::default(),
            stats_sample: Default::default(),
        }))
    }

    /// Apply filters/search/sort. Reads only the columns involved.
    pub fn build(dataset: &Dataset, spec: ViewSpec) -> Job<View> {
        let dataset = dataset.clone();
        dataset
            .engine
            .clone()
            .run(Lane::Task, move |conn| build_now(conn, dataset, spec))
    }

    pub fn is_identity(&self) -> bool {
        matches!(*self.access.read(), Access::Identity)
    }

    /// Consecutive rows of this view live far apart in the source (a sorted view
    /// read through an index), so each fetched row is comparatively expensive.
    pub fn is_scattered(&self) -> bool {
        matches!(*self.access.read(), Access::Index { .. }) && !self.spec.sort.is_empty()
    }

    pub fn is_materialized(&self) -> bool {
        matches!(*self.access.read(), Access::Materialized { .. })
    }

    pub fn columns(&self) -> &[ColumnInfo] {
        &self.dataset.columns
    }

    /// Fetch display text for a block of rows and columns.
    pub fn fetch_page(&self, request: PageRequest) -> Job<Page> {
        let view = self.clone();
        self.dataset.engine.run(Lane::Interactive, move |conn| {
            view.dataset.prepare_remote(conn)?;
            view.fetch_page_now(conn, &request)
        })
    }

    /// Full, untruncated values for copying and inspection. At most `limit` rows.
    pub fn fetch_values(
        &self,
        rows: Range<u64>,
        columns: Vec<usize>,
        limit: usize,
    ) -> Job<Vec<Vec<Option<String>>>> {
        let view = self.clone();
        self.dataset.engine.run(Lane::Interactive, move |conn| {
            view.dataset.prepare_remote(conn)?;
            let end = rows.end.min(rows.start + limit as u64).min(view.row_count);
            if end <= rows.start {
                return Ok(Vec::new());
            }
            let cols: Vec<&ColumnInfo> = columns
                .iter()
                .filter_map(|&ix| view.dataset.columns.get(ix))
                .collect();
            let exprs: Vec<String> = cols
                .iter()
                .enumerate()
                .map(|(i, c)| format!("CAST({} AS VARCHAR) AS c{i}", qualified(Some("s"), &c.name)))
                .collect();
            let sql = view.rows_sql(conn, &exprs.join(", "), rows.start, end - rows.start)?;
            let mut stmt = conn.prepare(&sql)?;
            let mut out = Vec::new();
            for batch in stmt.query_arrow([])? {
                for row in 0..batch.num_rows() {
                    let mut values = Vec::with_capacity(cols.len());
                    for col in 0..batch.num_columns() {
                        let array = batch.column(col);
                        values.push(string_value(array.as_ref(), row));
                    }
                    out.push(values);
                }
            }
            Ok(out)
        })
    }

    /// Rows as JSON objects (typed values, nested structures preserved).
    pub fn fetch_json(&self, rows: Range<u64>, columns: Vec<usize>, limit: usize) -> Job<Vec<String>> {
        let view = self.clone();
        self.dataset.engine.run(Lane::Interactive, move |conn| {
            view.dataset.prepare_remote(conn)?;
            let end = rows.end.min(rows.start + limit as u64).min(view.row_count);
            if end <= rows.start {
                return Ok(Vec::new());
            }
            let exprs: Vec<String> = columns
                .iter()
                .filter_map(|&ix| view.dataset.columns.get(ix))
                .map(|c| {
                    format!(
                        "{} AS {}",
                        qualified(Some("s"), &c.name),
                        crate::sql::ident(&c.name)
                    )
                })
                .collect();
            let inner = view.rows_sql(conn, &exprs.join(", "), rows.start, end - rows.start)?;
            let sql = format!("SELECT to_json(r)::VARCHAR FROM ({inner}) r");
            let mut stmt = conn.prepare(&sql)?;
            let values = stmt
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            Ok(values)
        })
    }

    /// SQL that yields this view's rows (all columns, in view order). For export,
    /// statistics and "open in SQL".
    pub fn select_sql(&self) -> String {
        let columns: Vec<String> = self
            .dataset
            .columns
            .iter()
            .map(|c| format!("{} AS {}", qualified(Some("s"), &c.name), crate::sql::ident(&c.name)))
            .collect();
        self.all_rows_sql(&columns.join(", "))
    }

    /// SQL for the rows of this view *without* guaranteeing order, cheaper for
    /// aggregates: filters apply directly to the source.
    pub(crate) fn unordered_source_sql(&self) -> Result<String> {
        let src = self.dataset.base.src();
        match &*self.access.read() {
            Access::Identity => Ok(format!("SELECT * FROM {src}")),
            Access::Materialized { table } => Ok(format!("SELECT * FROM {table}")),
            Access::Index { .. } => {
                let where_clause = self.spec.where_clause(&self.dataset.columns, None)?;
                Ok(match where_clause {
                    Some(w) => format!("SELECT * FROM {src} WHERE {w}"),
                    None => format!("SELECT * FROM {src}"),
                })
            }
        }
    }

    /// Copy an index view into DuckDB when it's small enough, so later pages are instant.
    /// Returns immediately for views that don't benefit.
    pub fn materialize_if_small(&self) -> Option<Job<()>> {
        if !self.is_small_index() {
            return None;
        }
        let view = self.clone();
        Some(self.dataset.engine.run(Lane::Background, move |conn| {
            view.dataset.prepare_remote(conn)?;
            view.materialize_now(conn)
        }))
    }

    /// An index view small enough to copy into DuckDB.
    pub(crate) fn is_small_index(&self) -> bool {
        matches!(*self.access.read(), Access::Index { .. })
            && self.row_count <= self.dataset.engine.settings().materialize_limit_rows
    }

    pub(crate) fn is_index(&self) -> bool {
        matches!(*self.access.read(), Access::Index { .. })
    }

    /// Copy the view's rows into a table (idempotent; safe to call concurrently).
    pub(crate) fn materialize_now(&self, conn: &Connection) -> Result<()> {
        let _guard = self.materialize_lock.lock();
        if !self.is_index() {
            return Ok(());
        }
        let table = format!("pq_mat_{}", self.dataset.engine.next_id());
        let sql = self.select_sql();
        conn.execute_batch(&format!("CREATE TABLE {table} AS {sql}"))?;
        let old = std::mem::replace(&mut *self.access.write(), Access::Materialized { table });
        if let Access::Index { table: old_table } = old {
            self.retired.lock().push(old_table);
        }
        Ok(())
    }

    /// A table holding a random sample of this (index) view's rows, created once.
    pub(crate) fn index_sample(&self, conn: &Connection, rows: u64) -> Result<Option<(String, u64)>> {
        let mut guard = self.stats_sample.lock();
        if let Some(sample) = guard.as_ref() {
            return Ok(Some(sample.clone()));
        }
        let index = match &*self.access.read() {
            Access::Index { table } => table.clone(),
            _ => return Ok(None),
        };
        let columns: Vec<String> = self
            .dataset
            .columns
            .iter()
            .map(|c| format!("{} AS {}", qualified(Some("s"), &c.name), crate::sql::ident(&c.name)))
            .collect();
        let slice = format!("(SELECT rowid AS pos, * FROM {index} USING SAMPLE reservoir({rows} ROWS) REPEATABLE (7))");
        let select = self.index_join_sql(&columns.join(", "), &slice);
        let table = format!("pq_vsmp_{}", self.dataset.engine.next_id());
        conn.execute_batch(&format!("CREATE TABLE {table} AS {select}"))?;
        let count: i64 = conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))?;
        let sample = (table, count.max(0) as u64);
        *guard = Some(sample.clone());
        Ok(Some(sample))
    }

    fn fetch_page_now(&self, conn: &Connection, request: &PageRequest) -> Result<Page> {
        let columns = &self.dataset.columns;
        let col_range = request.columns.start.min(columns.len())..request.columns.end.min(columns.len());
        let row_start = request.rows.start.min(self.row_count);
        let row_end = request.rows.end.min(self.row_count);
        let width = col_range.len();
        if row_end <= row_start || width == 0 {
            return Ok(Page {
                rows: row_start..row_start,
                columns: col_range,
                cells: Vec::new(),
            });
        }
        let inner_exprs: Vec<String> = columns[col_range.clone()]
            .iter()
            .enumerate()
            .map(|(i, c)| format!("CAST({} AS VARCHAR) AS v{i}", qualified(Some("s"), &c.name)))
            .collect();
        let outer_exprs: Vec<String> = (0..width)
            .map(|i| {
                format!(
                    "CASE WHEN strlen(v{i}) > {limit} THEN left(v{i}, {limit}) || '…' ELSE v{i} END",
                    limit = DISPLAY_TEXT_LIMIT
                )
            })
            .collect();
        let inner = self.rows_sql(conn, &inner_exprs.join(", "), row_start, row_end - row_start)?;
        let sql = format!("SELECT {} FROM ({inner})", outer_exprs.join(", "));

        let expected = (row_end - row_start) as usize;
        let mut cells: Vec<Option<Arc<str>>> = Vec::with_capacity(expected * width);
        let mut stmt = conn.prepare(&sql)?;
        for batch in stmt.query_arrow([])? {
            let arrays: Vec<_> = (0..batch.num_columns()).map(|c| batch.column(c).clone()).collect();
            for row in 0..batch.num_rows() {
                for array in &arrays {
                    cells.push(string_value(array.as_ref(), row).map(|s| display_text(&s)));
                }
            }
        }
        let got = cells.len() / width;
        Ok(Page {
            rows: row_start..row_start + got as u64,
            columns: col_range,
            cells,
        })
    }

    /// `SELECT {projection}` over rows `[offset, offset+limit)` of the view, in order.
    /// The projection refers to source columns through alias `s`.
    fn rows_sql(&self, conn: &Connection, projection: &str, offset: u64, limit: u64) -> Result<String> {
        let src = self.dataset.base.src();
        let table = match &*self.access.read() {
            Access::Identity => {
                return Ok(format!("SELECT {projection} FROM {src} s LIMIT {limit} OFFSET {offset}"));
            }
            Access::Materialized { table } => {
                return Ok(format!("SELECT {projection} FROM {table} s LIMIT {limit} OFFSET {offset}"));
            }
            Access::Index { table } => table.clone(),
        };
        // Look up the slice of row ids first, then fetch exactly those rows with
        // literal `IN` filters. DuckDB pushes those into the Parquet scan and only
        // decodes the vectors holding the rows, which is several times faster than
        // joining against the index when the rows are scattered (e.g. after a sort).
        let has_fid = matches!(self.dataset.base, Base::Parquet { files_table: Some(_), .. });
        let slice_sql = if has_fid {
            format!("SELECT rowid AS pos, fid, rid FROM {table} LIMIT {limit} OFFSET {offset}")
        } else {
            format!("SELECT rowid AS pos, 0 AS fid, rid FROM {table} LIMIT {limit} OFFSET {offset}")
        };
        let mut stmt = conn.prepare(&slice_sql)?;
        let slice: Vec<(i64, i64, i64)> = stmt
            .query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?)))?
            .collect::<std::result::Result<_, _>>()?;
        Ok(self.index_rows_sql(projection, &slice))
    }

    /// SQL selecting the rows `(pos, file id, row id)` in `pos` order.
    fn index_rows_sql(&self, projection: &str, slice: &[(i64, i64, i64)]) -> String {
        if slice.is_empty() {
            let src = self.dataset.base.src();
            return format!("SELECT {projection} FROM {src} s LIMIT 0");
        }
        let values = |rows: &[&(i64, i64, i64)]| -> (String, String) {
            let values: Vec<String> = rows.iter().map(|(pos, _, rid)| format!("({pos}, {rid})")).collect();
            let ids: Vec<String> = rows.iter().map(|(_, _, rid)| rid.to_string()).collect();
            (values.join(", "), ids.join(", "))
        };
        match &self.dataset.base {
            Base::Parquet {
                meta,
                files_table,
                row_ids: true,
                ..
            } => {
                let mut by_file: std::collections::BTreeMap<i64, Vec<&(i64, i64, i64)>> = Default::default();
                for row in slice {
                    by_file.entry(row.1).or_default().push(row);
                }
                let branches: Vec<String> = by_file
                    .iter()
                    .map(|(fid, rows)| {
                        let (values, ids) = values(rows);
                        let file_filter = match (files_table, self.dataset.files.get(*fid as usize)) {
                            (Some(_), Some(file)) => {
                                format!("s.{FILENAME_COLUMN} = {} AND ", literal(&file.path))
                            }
                            _ => String::new(),
                        };
                        format!(
                            "SELECT s.*, i.pos AS __parquetry_pos FROM {meta} s JOIN (VALUES {values}) i(pos, rid) ON s.file_row_number = i.rid WHERE {file_filter}s.file_row_number IN ({ids})"
                        )
                    })
                    .collect();
                format!(
                    "SELECT {projection} FROM ({}) s ORDER BY s.__parquetry_pos",
                    branches.join(" UNION ALL ")
                )
            }
            Base::Table { table, .. } => {
                let rows: Vec<&(i64, i64, i64)> = slice.iter().collect();
                let (values, ids) = values(&rows);
                format!(
                    "SELECT {projection} FROM {table} s JOIN (VALUES {values}) i(pos, rid) ON s.rowid = i.rid WHERE s.rowid IN ({ids}) ORDER BY i.pos"
                )
            }
            Base::Relation { .. } | Base::Parquet { row_ids: false, .. } => {
                unreachable!("views without row ids are always materialized")
            }
        }
    }

    fn all_rows_sql(&self, projection: &str) -> String {
        let src = self.dataset.base.src();
        match &*self.access.read() {
            Access::Identity => format!("SELECT {projection} FROM {src} s"),
            Access::Materialized { table } => format!("SELECT {projection} FROM {table} s"),
            Access::Index { table } => {
                let slice = format!("(SELECT rowid AS pos, * FROM {table})");
                self.index_join_sql(projection, &slice)
            }
        }
    }

    fn index_join_sql(&self, projection: &str, slice: &str) -> String {
        match &self.dataset.base {
            Base::Parquet {
                meta,
                files_table: None,
                row_ids: true,
                ..
            } => format!(
                "SELECT {projection} FROM {meta} s JOIN {slice} i ON s.file_row_number = i.rid ORDER BY i.pos"
            ),
            Base::Parquet {
                meta,
                files_table: Some(files),
                row_ids: true,
                ..
            } => format!(
                "SELECT {projection} FROM {meta} s JOIN (SELECT x.pos, f.file, x.rid FROM {slice} x JOIN {files} f USING (fid)) i ON s.{FILENAME_COLUMN} = i.file AND s.file_row_number = i.rid ORDER BY i.pos"
            ),
            Base::Table { table, .. } => format!(
                "SELECT {projection} FROM {table} s JOIN {slice} i ON s.rowid = i.rid ORDER BY i.pos"
            ),
            Base::Relation { .. } | Base::Parquet { row_ids: false, .. } => {
                unreachable!("views without row ids are always materialized")
            }
        }
    }
}

fn build_now(conn: &Connection, dataset: Dataset, spec: ViewSpec) -> Result<View> {
    let started = Instant::now();
    if spec.is_identity() {
        return Ok(View::identity(&dataset));
    }
    dataset.prepare_remote(conn)?;
    validate_filter_values(conn, &dataset, &spec)?;
    let columns = &dataset.columns;
    let engine = &dataset.engine;
    let id = engine.next_id();
    let settings = engine.settings();

    let (access, truncated) = match &dataset.base {
        Base::Parquet {
            meta,
            files_table,
            row_ids: true,
            ..
        } => {
            let table = format!("pq_idx_{id}");
            let where_clause = spec
                .where_clause(columns, Some("s"))?
                .map(|w| format!("WHERE {w}"))
                .unwrap_or_default();
            let order = spec.order_clause(columns, Some("s"))?;
            let sql = match files_table {
                None => {
                    let order = match order {
                        Some(o) => format!("{o}, s.file_row_number"),
                        None => "s.file_row_number".to_string(),
                    };
                    format!(
                        "CREATE TABLE {table} AS SELECT s.file_row_number AS rid FROM {meta} s {where_clause} ORDER BY {order}"
                    )
                }
                Some(files) => {
                    let order = match order {
                        Some(o) => format!("{o}, f.fid, s.file_row_number"),
                        None => "f.fid, s.file_row_number".to_string(),
                    };
                    format!(
                        "CREATE TABLE {table} AS SELECT f.fid, s.file_row_number AS rid FROM {meta} s JOIN {files} f ON s.{FILENAME_COLUMN} = f.file {where_clause} ORDER BY {order}"
                    )
                }
            };
            conn.execute_batch(&sql)?;
            (Access::Index { table }, false)
        }
        Base::Table { table: source, .. } => {
            let table = format!("pq_idx_{id}");
            let where_clause = spec
                .where_clause(columns, None)?
                .map(|w| format!("WHERE {w}"))
                .unwrap_or_default();
            let order = match spec.order_clause(columns, None)? {
                Some(o) => format!("{o}, rowid"),
                None => "rowid".to_string(),
            };
            conn.execute_batch(&format!(
                "CREATE TABLE {table} AS SELECT rowid AS rid FROM {source} {where_clause} ORDER BY {order}"
            ))?;
            (Access::Index { table }, false)
        }
        Base::Relation { src } | Base::Parquet { src, row_ids: false, .. } => {
            let table = format!("pq_mat_{id}");
            let where_clause = spec
                .where_clause(columns, None)?
                .map(|w| format!("WHERE {w}"))
                .unwrap_or_default();
            let order = spec
                .order_clause(columns, None)?
                .map(|o| format!("ORDER BY {o}"))
                .unwrap_or_default();
            let cap = settings.result_limit_rows;
            conn.execute_batch(&format!(
                "CREATE TABLE {table} AS SELECT * FROM {src} {where_clause} {order} LIMIT {cap}"
            ))?;
            let count: i64 = conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))?;
            (Access::Materialized { table }, count as u64 >= cap)
        }
    };

    let table = match &access {
        Access::Index { table } | Access::Materialized { table } => table.clone(),
        Access::Identity => unreachable!(),
    };
    let row_count: i64 = conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))?;
    Ok(View(Arc::new(ViewInner {
        id,
        dataset,
        spec,
        row_count: row_count.max(0) as u64,
        truncated,
        build_millis: started.elapsed().as_millis() as u64,
        access: RwLock::new(access),
        retired: Default::default(),
        materialize_lock: Default::default(),
        stats_sample: Default::default(),
    })))
}

/// Give a friendly error for values that don't fit the column type ("abc" for an int).
fn validate_filter_values(conn: &Connection, dataset: &Dataset, spec: &ViewSpec) -> Result<()> {
    for filter in &spec.filters {
        let Some(column) = dataset.columns.iter().find(|c| c.name == filter.column) else {
            return Err(Error::other(format!("There’s no column named “{}”", filter.column)));
        };
        for value in filter.typed_values(column) {
            let ok: bool = conn
                .query_row(
                    &format!("SELECT TRY_CAST({} AS {}) IS NOT NULL", literal(&value), column.sql_type),
                    [],
                    |r| r.get(0),
                )
                .unwrap_or(false);
            if !ok {
                return Err(Error::other(format!(
                    "“{value}” isn’t a valid {} for {}",
                    column.type_label(),
                    column.name
                )));
            }
        }
    }
    if !spec.where_sql.trim().is_empty() {
        // Surface syntax and name errors before a long scan starts.
        let src = dataset.base.src();
        conn.execute_batch(&format!(
            "EXPLAIN SELECT * FROM {src} WHERE ({})",
            spec.where_sql.trim()
        ))?;
    }
    Ok(())
}

fn string_value(array: &dyn Array, row: usize) -> Option<String> {
    if array.is_null(row) {
        return None;
    }
    if let Some(strings) = array.as_string_opt::<i32>() {
        return Some(strings.value(row).to_string());
    }
    if let Some(strings) = array.as_string_opt::<i64>() {
        return Some(strings.value(row).to_string());
    }
    if let Some(view) = array.as_string_view_opt() {
        return Some(view.value(row).to_string());
    }
    arrow::util::display::ArrayFormatter::try_new(array, &Default::default())
        .ok()
        .map(|f| f.value(row).to_string())
}

/// Single-line text for a grid cell: control characters become visible symbols.
pub fn display_text(value: &str) -> Arc<str> {
    if !value.chars().any(|c| c.is_control()) {
        return Arc::from(value);
    }
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\n' => out.push('↵'),
            '\r' => {}
            '\t' => out.push('→'),
            c if c.is_control() => out.push('·'),
            c => out.push(c),
        }
    }
    Arc::from(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_text_is_single_line() {
        assert_eq!(&*display_text("a\nb\tc\r"), "a↵b→c");
        assert_eq!(&*display_text("plain"), "plain");
    }
}
