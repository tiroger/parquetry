//! Parquetry's data engine: DuckDB-backed reading, paging, statistics and SQL for
//! Parquet and friends, with no UI dependencies.

mod codegen;
mod dataset;
mod diff;
mod engine;
mod error;
mod export;
mod filter;
mod metadata;
mod query;
mod s3;
mod source;
pub mod sql;
mod stats;
mod types;
mod view;

pub use codegen::{CodeFlavor, CodeOptions, python_packages, view_code};
pub use dataset::{Dataset, DatasetOrigin, FileEntry, ParquetSummary};
pub use diff::{ColumnChanges, CompareOptions, Comparison, TypeChange, compare};
pub use engine::{Canceller, Engine, EnginePaths, EngineSettings, Job, Lane, S3Settings};
pub use error::{Error, Result};
pub use export::{CopyFormat, ExportFormat, ExportOutcome, export_view, format_cells};
pub use filter::{Filter, FilterOp, SortKey, ViewSpec};
pub use metadata::{MetadataTable, metadata_table};
pub use query::{QueryOutcome, SqlTable, run_sql, split_statements, table_name_for};
pub use s3::{CredentialIssue, S3Entry, S3Url, credential_issue, is_remote};
pub use source::{Format, SourceSpec, expand_home as expand_home_path, format_from_extension};
pub use stats::{
    ChartKind, ColumnSummary, HistogramBin, StatsMode, TopValue, ValueCounts, format_epoch_micros,
    format_number, summarize, value_counts,
};
pub use types::{ColumnInfo, ColumnKind, short_type_label};
pub use view::{DISPLAY_TEXT_LIMIT, Page, PageRequest, SelectionStats, View, display_text};

impl S3Settings {
    /// Whether any S3 setting differs from the defaults.
    pub fn is_customized(&self) -> bool {
        *self != S3Settings::default()
    }
}

impl Engine {
    /// List buckets or a prefix in S3.
    pub fn s3_list(&self, url: String) -> Job<Vec<S3Entry>> {
        let engine = self.clone();
        self.run(Lane::Task, move |_| engine.inner.s3.list(&url))
    }

    /// Forget cached S3 credentials, so the next access picks up new ones (after
    /// `aws sso login`, say).
    pub fn refresh_s3_credentials(&self) {
        self.inner.s3.reconfigure(self.settings().s3);
    }
}
