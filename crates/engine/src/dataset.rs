//! An opened dataset: its schema, size, and how to read it.

use std::sync::Arc;
use std::time::Instant;

use duckdb::Connection;

use crate::engine::{Engine, Job, Lane};
use crate::error::{Error, Result};
use crate::source::{self, Format, Resolved, SourceSpec};
use crate::sql::{literal, literal_list};
use crate::types::ColumnInfo;

/// One file behind a dataset.
#[derive(Debug, Clone, PartialEq)]
pub struct FileEntry {
    pub path: String,
    pub rows: Option<u64>,
    pub bytes: Option<u64>,
    pub row_groups: Option<u64>,
}

/// Facts from Parquet footers, available without reading any data.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ParquetSummary {
    pub created_by: Option<String>,
    pub format_version: Option<i64>,
    pub row_groups: u64,
    pub compression: Vec<String>,
}

/// Name of the column holding each row's file, chosen not to collide with data columns.
pub(crate) const FILENAME_COLUMN: &str = "__parquetry_file";

/// Where a dataset's rows come from, as DuckDB objects.
#[derive(Debug)]
pub(crate) enum Base {
    /// Parquet files read in place. Rows are identified by (file, row number).
    Parquet {
        /// A view over the files, user columns only.
        src: String,
        /// `read_parquet(...)` with `filename` and `file_row_number` columns added.
        meta: String,
        /// Maps file ids to file names when there are several files.
        files_table: Option<String>,
        /// False when a file has its own `file_row_number` column, which prevents
        /// DuckDB from adding row numbers; views are then materialized instead.
        row_ids: bool,
    },
    /// A DuckDB table we created (CSV/JSON/Arrow imports, query results).
    Table { table: String, owned: bool },
    /// A relation with no stable row identity (Delta, Iceberg).
    Relation { src: String },
}

impl Base {
    /// Name of the view or table holding the user-visible columns.
    pub(crate) fn src(&self) -> &str {
        match self {
            Base::Parquet { src, .. } => src,
            Base::Table { table, .. } => table,
            Base::Relation { src } => src,
        }
    }
}

/// What kind of thing a dataset is, for presentation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DatasetOrigin {
    /// Opened from a file, folder, glob or URL.
    Source(SourceSpec),
    /// The result of a SQL query.
    Query(String),
    /// A derived table (metadata listing, comparison result, ...).
    Derived(String),
}

pub struct DatasetInner {
    pub id: u64,
    pub origin: DatasetOrigin,
    pub name: String,
    pub format: Option<Format>,
    pub columns: Vec<ColumnInfo>,
    pub row_count: u64,
    pub files: Vec<FileEntry>,
    pub total_bytes: Option<u64>,
    pub parquet: Option<ParquetSummary>,
    /// Human notes about how the data was read (merged schemas, truncation, ...).
    pub notes: Vec<String>,
    pub open_millis: u64,
    pub(crate) base: Base,
    pub(crate) remote_urls: Vec<String>,
    pub(crate) engine: Engine,
    /// A local copy of an even sample of rows, created on first use by statistics.
    pub(crate) sample: parking_lot::Mutex<Option<SampleTable>>,
}

#[derive(Debug, Clone)]
pub(crate) struct SampleTable {
    pub(crate) table: String,
    pub(crate) rows: u64,
}

/// A handle to an opened dataset. Cheap to clone; DuckDB objects are dropped with the last handle.
#[derive(Clone)]
pub struct Dataset(pub(crate) Arc<DatasetInner>);

impl std::ops::Deref for Dataset {
    type Target = DatasetInner;
    fn deref(&self) -> &DatasetInner {
        &self.0
    }
}

impl std::fmt::Debug for Dataset {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Dataset")
            .field("name", &self.name)
            .field("rows", &self.row_count)
            .field("columns", &self.columns.len())
            .finish()
    }
}

impl Drop for DatasetInner {
    fn drop(&mut self) {
        let mut statements = Vec::new();
        if let Some(sample) = self.sample.lock().take() {
            statements.push(format!("DROP TABLE IF EXISTS {};", sample.table));
        }
        match &self.base {
            Base::Parquet {
                src, files_table, ..
            } => {
                statements.push(format!("DROP VIEW IF EXISTS {src};"));
                if let Some(files) = files_table {
                    statements.push(format!("DROP TABLE IF EXISTS {files};"));
                }
            }
            Base::Table { table, owned } => {
                if *owned {
                    statements.push(format!("DROP TABLE IF EXISTS {table};"));
                }
            }
            Base::Relation { src } => statements.push(format!("DROP VIEW IF EXISTS {src};")),
        }
        self.engine.run_detached(statements.join(" "));
    }
}

impl Dataset {
    /// Open a file, folder, glob or URL.
    pub fn open(engine: &Engine, spec: SourceSpec) -> Job<Dataset> {
        let engine_for_job = engine.clone();
        engine.run(Lane::Task, move |conn| open_now(&engine_for_job, conn, spec))
    }

    /// Wrap an existing DuckDB table (created by the engine) as a dataset.
    pub(crate) fn from_table(
        engine: &Engine,
        conn: &Connection,
        table: String,
        name: String,
        origin: DatasetOrigin,
        notes: Vec<String>,
    ) -> Result<Dataset> {
        let columns = describe(conn, &table)?;
        let row_count: i64 = conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))?;
        Ok(Dataset(Arc::new(DatasetInner {
            id: engine.next_id(),
            origin,
            name,
            format: None,
            columns,
            row_count: row_count.max(0) as u64,
            files: Vec::new(),
            total_bytes: None,
            parquet: None,
            notes,
            open_millis: 0,
            base: Base::Table { table, owned: true },
            remote_urls: Vec::new(),
            engine: engine.clone(),
            sample: Default::default(),
        })))
    }

    pub fn is_remote(&self) -> bool {
        !self.remote_urls.is_empty()
    }

    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    /// The DuckDB view or table with this dataset's columns, usable in SQL.
    pub fn relation_name(&self) -> &str {
        self.base.src()
    }

    pub fn source(&self) -> Option<&SourceSpec> {
        match &self.origin {
            DatasetOrigin::Source(spec) => Some(spec),
            _ => None,
        }
    }

    pub fn column_index(&self, name: &str) -> Option<usize> {
        self.columns.iter().position(|c| c.name == name)
    }

    pub fn is_parquet(&self) -> bool {
        matches!(self.base, Base::Parquet { .. })
    }

    /// Paths or URLs of the Parquet files (empty for other kinds).
    pub fn parquet_files(&self) -> Vec<String> {
        if self.is_parquet() {
            self.files.iter().map(|f| f.path.clone()).collect()
        } else {
            Vec::new()
        }
    }

    /// Refresh S3 credentials for this dataset's buckets if they are about to expire.
    pub(crate) fn prepare_remote(&self, conn: &Connection) -> Result<()> {
        for url in &self.remote_urls {
            self.engine.inner.s3.prepare_duckdb(&self.engine, conn, url)?;
        }
        Ok(())
    }
}

pub(crate) fn describe(conn: &Connection, relation: &str) -> Result<Vec<ColumnInfo>> {
    let mut stmt = conn.prepare(&format!("DESCRIBE SELECT * FROM {relation}"))?;
    let columns = stmt
        .query_map([], |row| {
            Ok(ColumnInfo::new(row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(columns)
}

fn open_now(engine: &Engine, conn: &Connection, spec: SourceSpec) -> Result<Dataset> {
    let started = Instant::now();
    let resolved = source::resolve(engine, conn, &spec)?;
    let id = engine.next_id();
    let name = spec.display_name();
    let mut notes = Vec::new();
    let remote_urls: Vec<String> = match &resolved {
        Resolved::Files { files, .. } => files
            .iter()
            .filter(|f| crate::s3::is_remote(f))
            .take(1)
            .cloned()
            .collect(),
        Resolved::Table { root, .. } => {
            if crate::s3::is_remote(root) {
                vec![root.clone()]
            } else {
                Vec::new()
            }
        }
    };

    let (format, base, files, parquet, row_count, total_bytes) = match resolved {
        Resolved::Files {
            format: Format::Parquet,
            files,
        } => open_parquet(conn, id, files, &mut notes)?,
        Resolved::Files { format, files } => {
            let table = format!("pq_tbl_{id}");
            import_files(engine, conn, format, &files, &table)?;
            let rows: i64 = conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))?;
            let (entries, bytes) = local_file_entries(&files);
            if files.len() > 1 {
                notes.push(format!(
                    "{} files combined by column name",
                    files.len()
                ));
            }
            (
                format,
                Base::Table { table, owned: true },
                entries,
                None,
                rows.max(0) as u64,
                bytes,
            )
        }
        Resolved::Table { format, root } => {
            let src = format!("pq_src_{id}");
            let relation = match format {
                Format::Delta => {
                    engine.ensure_extension(conn, "delta")?;
                    format!("delta_scan({})", literal(&root))
                }
                Format::Iceberg => {
                    engine.ensure_extension(conn, "iceberg")?;
                    format!("iceberg_scan({}, allow_moved_paths = true)", literal(&root))
                }
                _ => unreachable!("only table formats resolve to a root"),
            };
            conn.execute_batch(&format!("CREATE OR REPLACE VIEW {src} AS SELECT * FROM {relation}"))?;
            let rows: i64 = conn.query_row(&format!("SELECT count(*) FROM {src}"), [], |r| r.get(0))?;
            (
                format,
                Base::Relation { src },
                Vec::new(),
                None,
                rows.max(0) as u64,
                None,
            )
        }
    };

    let mut columns = describe(conn, base.src())?;
    if let (Base::Parquet { .. }, Some(first)) = (&base, files.first()) {
        annotate_parquet_types(conn, &first.path, &mut columns);
    }

    Ok(Dataset(Arc::new(DatasetInner {
        id,
        origin: DatasetOrigin::Source(spec),
        name,
        format: Some(format),
        columns,
        row_count,
        files,
        total_bytes,
        parquet,
        notes,
        open_millis: started.elapsed().as_millis() as u64,
        base,
        remote_urls,
        engine: engine.clone(),
        sample: Default::default(),
    })))
}

type Opened = (
    Format,
    Base,
    Vec<FileEntry>,
    Option<ParquetSummary>,
    u64,
    Option<u64>,
);

fn open_parquet(
    conn: &Connection,
    id: u64,
    files: Vec<String>,
    notes: &mut Vec<String>,
) -> Result<Opened> {
    let list = literal_list(&files);
    let multi = files.len() > 1;
    let options = if multi { ", union_by_name = true" } else { "" };
    let relation = format!("read_parquet({list}{options})");
    let meta = format!(
        "read_parquet({list}{options}, filename = '{FILENAME_COLUMN}', file_row_number = true)"
    );
    let src = format!("pq_src_{id}");
    conn.execute_batch(&format!("CREATE OR REPLACE VIEW {src} AS SELECT * FROM {relation}"))?;
    let row_ids = !describe(conn, &src)?
        .iter()
        .any(|c| c.name == "file_row_number");

    // Footers only: row counts, sizes, row groups, writer.
    let mut entries = Vec::with_capacity(files.len());
    let mut summary = ParquetSummary::default();
    {
        let mut stmt = conn.prepare(&format!(
            "SELECT file_name, num_rows, num_row_groups, file_size_bytes, created_by, format_version
             FROM parquet_file_metadata({list})"
        ))?;
        let mut rows = stmt.query([])?;
        let mut by_name = std::collections::HashMap::new();
        while let Some(row) = rows.next()? {
            let name: String = row.get(0)?;
            let num_rows: Option<i64> = row.get(1)?;
            let row_groups: Option<i64> = row.get(2)?;
            let bytes: Option<i64> = row.get(3).ok().flatten();
            let created_by: Option<String> = row.get(4)?;
            let version: Option<i64> = row.get(5).ok().flatten();
            if summary.created_by.is_none() {
                summary.created_by = created_by;
            }
            if summary.format_version.is_none() {
                summary.format_version = version;
            }
            summary.row_groups += row_groups.unwrap_or(0).max(0) as u64;
            by_name.insert(
                name,
                (
                    num_rows.map(|v| v.max(0) as u64),
                    bytes.map(|v| v.max(0) as u64),
                    row_groups.map(|v| v.max(0) as u64),
                ),
            );
        }
        for path in &files {
            let (rows, bytes, row_groups) = by_name.get(path).cloned().unwrap_or((None, None, None));
            entries.push(FileEntry {
                path: path.clone(),
                rows,
                bytes,
                row_groups,
            });
        }
    }
    let row_count: u64 = if entries.iter().all(|e| e.rows.is_some()) {
        entries.iter().map(|e| e.rows.unwrap_or(0)).sum()
    } else {
        let rows: i64 = conn.query_row(&format!("SELECT count(*) FROM {src}"), [], |r| r.get(0))?;
        rows.max(0) as u64
    };
    let total_bytes = if entries.iter().all(|e| e.bytes.is_some()) {
        Some(entries.iter().map(|e| e.bytes.unwrap_or(0)).sum())
    } else {
        None
    };
    if let Some(first) = files.first() {
        let mut stmt = conn.prepare(&format!(
            "SELECT DISTINCT compression FROM parquet_metadata({}) WHERE compression IS NOT NULL ORDER BY 1",
            literal(first)
        ))?;
        summary.compression = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .filter_map(|r| r.ok())
            .collect();
    }

    let files_table = if multi {
        let table = format!("pq_files_{id}");
        conn.execute_batch(&format!(
            "CREATE OR REPLACE TABLE {table} AS SELECT (generate_subscripts(l, 1) - 1)::INTEGER AS fid, unnest(l) AS file FROM (SELECT {list} AS l)"
        ))?;
        // Different schemas across files are merged by name; say so when it happens.
        let distinct_schemas: i64 = conn
            .query_row(
                &format!(
                    "SELECT count(DISTINCT cols) FROM (SELECT file_name, string_agg(name || ':' || coalesce(type, ''), ',' ORDER BY name) AS cols FROM parquet_schema({list}) GROUP BY file_name)"
                ),
                [],
                |r| r.get(0),
            )
            .unwrap_or(1);
        if distinct_schemas > 1 {
            notes.push("Files have different schemas; columns were merged by name".into());
        }
        Some(table)
    } else {
        None
    };

    Ok((
        Format::Parquet,
        Base::Parquet {
            src,
            meta,
            files_table,
            row_ids,
        },
        entries,
        Some(summary),
        row_count,
        total_bytes,
    ))
}

/// Add Parquet physical/logical types to top-level columns.
fn annotate_parquet_types(conn: &Connection, file: &str, columns: &mut [ColumnInfo]) {
    let query = format!(
        "SELECT name, type, coalesce(logical_type, converted_type) FROM parquet_schema({})",
        literal(file)
    );
    let Ok(mut stmt) = conn.prepare(&query) else {
        return;
    };
    let Ok(rows) = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, Option<String>>(1)?,
            r.get::<_, Option<String>>(2)?,
        ))
    }) else {
        return;
    };
    let schema: Vec<(String, Option<String>, Option<String>)> = rows.filter_map(|r| r.ok()).collect();
    for column in columns.iter_mut() {
        if let Some((_, physical, logical)) = schema.iter().find(|(name, _, _)| *name == column.name) {
            column.physical_type = physical.clone();
            column.logical_type = logical.clone();
        }
    }
}

fn local_file_entries(files: &[String]) -> (Vec<FileEntry>, Option<u64>) {
    let mut total = Some(0u64);
    let entries = files
        .iter()
        .map(|path| {
            let bytes = std::fs::metadata(path).ok().map(|m| m.len());
            total = match (total, bytes) {
                (Some(t), Some(b)) => Some(t + b),
                _ => None,
            };
            FileEntry {
                path: path.clone(),
                rows: None,
                bytes,
                row_groups: None,
            }
        })
        .collect();
    (entries, total)
}

/// Load non-Parquet files into a DuckDB table so they can be paged, sorted and filtered quickly.
fn import_files(
    engine: &Engine,
    conn: &Connection,
    format: Format,
    files: &[String],
    table: &str,
) -> Result<()> {
    let list = literal_list(files);
    let multi = files.len() > 1;
    let select = match format {
        Format::Csv => {
            let union = if multi { ", union_by_name = true" } else { "" };
            format!("SELECT * FROM read_csv({list}, auto_detect = true, sample_size = 20480{union})")
        }
        Format::Json => {
            let union = if multi { ", union_by_name = true" } else { "" };
            format!("SELECT * FROM read_json_auto({list}{union})")
        }
        Format::Excel => {
            engine.ensure_extension(conn, "excel")?;
            if multi {
                return Err(Error::other("Open one Excel workbook at a time"));
            }
            format!("SELECT * FROM read_xlsx({})", literal(&files[0]))
        }
        Format::Arrow => {
            return import_arrow(conn, files, table);
        }
        Format::Parquet | Format::Delta | Format::Iceberg => unreachable!(),
    };
    conn.execute_batch(&format!("CREATE OR REPLACE TABLE {table} AS {select}"))?;
    Ok(())
}

fn import_arrow(conn: &Connection, files: &[String], table: &str) -> Result<()> {
    use duckdb::vtab::arrow::{ArrowVTab, arrow_recordbatch_to_query_params};
    // Registration is per database; a second attempt fails harmlessly.
    let _ = conn.register_table_function::<ArrowVTab>("arrow");

    let mut created = false;
    for path in files {
        if crate::s3::is_remote(path) {
            return Err(Error::other("Arrow files can only be opened from disk"));
        }
        let batches = read_arrow_batches(path)?;
        let (schema, batches) = batches;
        let mut all = batches;
        if all.is_empty() {
            all.push(arrow::record_batch::RecordBatch::new_empty(schema));
        }
        for batch in all {
            let sql = if created {
                format!("INSERT INTO {table} BY NAME SELECT * FROM arrow(?, ?)")
            } else {
                format!("CREATE OR REPLACE TABLE {table} AS SELECT * FROM arrow(?, ?)")
            };
            let mut stmt = conn.prepare(&sql)?;
            stmt.execute(arrow_recordbatch_to_query_params(batch))?;
            created = true;
        }
    }
    Ok(())
}

type ArrowBatches = (
    arrow::datatypes::SchemaRef,
    Vec<arrow::record_batch::RecordBatch>,
);

fn read_arrow_batches(path: &str) -> Result<ArrowBatches> {
    use arrow::ipc::reader::{FileReader, StreamReader};
    let file = std::fs::File::open(path)?;
    match FileReader::try_new(std::io::BufReader::new(file), None) {
        Ok(reader) => {
            let schema = reader.schema();
            let batches = reader
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|e| Error::other(format!("Couldn’t read Arrow file: {e}")))?;
            Ok((schema, batches))
        }
        Err(_) => {
            let file = std::fs::File::open(path)?;
            let reader = StreamReader::try_new(std::io::BufReader::new(file), None)
                .map_err(|e| Error::other(format!("Not an Arrow IPC file: {e}")))?;
            let schema = reader.schema();
            let batches = reader
                .collect::<std::result::Result<Vec<_>, _>>()
                .map_err(|e| Error::other(format!("Couldn’t read Arrow stream: {e}")))?;
            Ok((schema, batches))
        }
    }
}
