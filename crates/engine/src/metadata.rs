//! Parquet file internals as browsable tables: files, row groups, column chunks,
//! per-column storage and key/value metadata.

use duckdb::Connection;

use crate::dataset::{Dataset, DatasetOrigin};
use crate::engine::{Job, Lane};
use crate::error::{Error, Result};
use crate::sql::literal_list;

/// Which metadata table to build.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MetadataTable {
    /// One row per column: compressed/uncompressed size, encodings, codecs, stats.
    ColumnStorage,
    /// One row per row group.
    RowGroups,
    /// One row per column chunk (row group × column).
    ColumnChunks,
    /// One row per file.
    Files,
    /// The Parquet schema tree.
    Schema,
    /// Key/value metadata (pandas, Arrow schema, Spark, ...).
    KeyValue,
}

impl MetadataTable {
    pub fn all() -> &'static [MetadataTable] {
        &[
            MetadataTable::ColumnStorage,
            MetadataTable::RowGroups,
            MetadataTable::ColumnChunks,
            MetadataTable::Files,
            MetadataTable::Schema,
            MetadataTable::KeyValue,
        ]
    }

    pub fn label(self) -> &'static str {
        match self {
            MetadataTable::ColumnStorage => "Column storage",
            MetadataTable::RowGroups => "Row groups",
            MetadataTable::ColumnChunks => "Column chunks",
            MetadataTable::Files => "Files",
            MetadataTable::Schema => "Parquet schema",
            MetadataTable::KeyValue => "Key/value metadata",
        }
    }
}

/// Files considered for per-chunk metadata; footers of more files are skipped.
const MAX_METADATA_FILES: usize = 250;

/// Build a metadata table for a Parquet dataset as its own dataset.
pub fn metadata_table(dataset: &Dataset, which: MetadataTable) -> Job<Dataset> {
    let dataset = dataset.clone();
    let engine = dataset.engine.clone();
    engine.run(Lane::Task, move |conn| build(conn, &dataset, which))
}

fn build(conn: &Connection, dataset: &Dataset, which: MetadataTable) -> Result<Dataset> {
    if !dataset.is_parquet() {
        return Err(Error::other("File metadata is only available for Parquet"));
    }
    dataset.prepare_remote(conn)?;
    let mut files = dataset.parquet_files();
    let mut notes = Vec::new();
    if files.len() > MAX_METADATA_FILES {
        notes.push(format!(
            "Showing the first {MAX_METADATA_FILES} of {} files",
            files.len()
        ));
        files.truncate(MAX_METADATA_FILES);
    }
    let list = literal_list(&files);
    let multi = files.len() > 1;
    let file_col = if multi { "file_name AS file, " } else { "" };
    let file_order = if multi { "file_name, " } else { "" };
    let select = match which {
        MetadataTable::ColumnStorage => format!(
            "SELECT path_in_schema AS column,
                    any_value(type) AS physical_type,
                    sum(total_compressed_size)::BIGINT AS compressed_bytes,
                    sum(total_uncompressed_size)::BIGINT AS uncompressed_bytes,
                    round(sum(total_uncompressed_size) / nullif(sum(total_compressed_size), 0), 2) AS compression_ratio,
                    round(100.0 * sum(total_compressed_size) / nullif(sum(sum(total_compressed_size)) OVER (), 0), 2) AS pct_of_data,
                    string_agg(DISTINCT compression, ', ') AS compression,
                    string_agg(DISTINCT encodings, ' | ') AS encodings,
                    sum(stats_null_count)::BIGINT AS null_count,
                    min(stats_min_value) AS min_value,
                    max(stats_max_value) AS max_value,
                    bool_or(dictionary_page_offset IS NOT NULL) AS has_dictionary,
                    bool_or(bloom_filter_offset IS NOT NULL) AS has_bloom_filter,
                    count(*)::BIGINT AS chunks
             FROM parquet_metadata({list})
             GROUP BY path_in_schema, column_id
             ORDER BY min(column_id)"
        ),
        MetadataTable::RowGroups => format!(
            "SELECT {file_col}row_group_id AS row_group,
                    any_value(row_group_num_rows)::BIGINT AS rows,
                    any_value(row_group_num_columns)::BIGINT AS columns,
                    sum(total_compressed_size)::BIGINT AS compressed_bytes,
                    sum(total_uncompressed_size)::BIGINT AS uncompressed_bytes,
                    round(sum(total_uncompressed_size) / nullif(sum(total_compressed_size), 0), 2) AS compression_ratio,
                    string_agg(DISTINCT compression, ', ') AS compression
             FROM parquet_metadata({list})
             GROUP BY file_name, row_group_id
             ORDER BY {file_order}row_group_id"
        ),
        MetadataTable::ColumnChunks => format!(
            "SELECT {file_col}row_group_id AS row_group, path_in_schema AS column, type AS physical_type,
                    num_values::BIGINT AS num_values, compression, encodings,
                    total_compressed_size::BIGINT AS compressed_bytes,
                    total_uncompressed_size::BIGINT AS uncompressed_bytes,
                    stats_null_count::BIGINT AS null_count,
                    stats_distinct_count::BIGINT AS distinct_count,
                    stats_min_value AS min_value, stats_max_value AS max_value,
                    min_is_exact, max_is_exact,
                    dictionary_page_offset::BIGINT AS dictionary_page_offset,
                    data_page_offset::BIGINT AS data_page_offset,
                    bloom_filter_length::BIGINT AS bloom_filter_bytes
             FROM parquet_metadata({list})
             ORDER BY {file_order}row_group_id, column_id"
        ),
        MetadataTable::Files => format!(
            "SELECT file_name AS file, num_rows::BIGINT AS rows, num_row_groups::BIGINT AS row_groups,
                    file_size_bytes::BIGINT AS bytes, footer_size::BIGINT AS footer_bytes,
                    created_by, format_version, encryption_algorithm
             FROM parquet_file_metadata({list})
             ORDER BY file_name"
        ),
        MetadataTable::Schema => format!(
            "SELECT {file_col}name, type AS physical_type, logical_type, converted_type,
                    repetition_type, num_children::BIGINT AS children, type_length::BIGINT AS type_length,
                    scale::BIGINT AS scale, precision::BIGINT AS precision, field_id::BIGINT AS field_id,
                    duckdb_type
             FROM parquet_schema({list})
             {where_first}",
            where_first = if multi {
                format!("WHERE file_name = {}", crate::sql::literal(&files[0]))
            } else {
                String::new()
            }
        ),
        MetadataTable::KeyValue => format!(
            "SELECT {file_col}decode(key) AS key,
                    CASE WHEN octet_length(value) > 20000 THEN '(' || octet_length(value) || ' bytes)'
                         ELSE try(decode(value)) END AS value,
                    octet_length(value)::BIGINT AS bytes
             FROM parquet_kv_metadata({list})"
        ),
    };
    let table = format!("pq_meta_{}", dataset.engine.next_id());
    conn.execute_batch(&format!("CREATE TABLE {table} AS {select}"))?;
    Dataset::from_table(
        &dataset.engine,
        conn,
        table,
        format!("{} · {}", dataset.name, which.label()),
        DatasetOrigin::Derived(which.label().to_string()),
        notes,
    )
}
