//! Writing views to files and formatting copied cells.

use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::engine::{Job, Lane};
use crate::error::{Error, Result};
use crate::sql::{ident, literal};
use crate::view::View;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ExportFormat {
    Parquet,
    Csv,
    Tsv,
    JsonLines,
    Json,
}

impl ExportFormat {
    pub fn all() -> &'static [ExportFormat] {
        &[
            ExportFormat::Parquet,
            ExportFormat::Csv,
            ExportFormat::Tsv,
            ExportFormat::JsonLines,
            ExportFormat::Json,
        ]
    }

    pub fn label(self) -> &'static str {
        match self {
            ExportFormat::Parquet => "Parquet",
            ExportFormat::Csv => "CSV",
            ExportFormat::Tsv => "TSV",
            ExportFormat::JsonLines => "JSON Lines",
            ExportFormat::Json => "JSON array",
        }
    }

    pub fn extension(self) -> &'static str {
        match self {
            ExportFormat::Parquet => "parquet",
            ExportFormat::Csv => "csv",
            ExportFormat::Tsv => "tsv",
            ExportFormat::JsonLines => "jsonl",
            ExportFormat::Json => "json",
        }
    }

    fn copy_options(self) -> &'static str {
        match self {
            ExportFormat::Parquet => "FORMAT parquet, COMPRESSION zstd",
            ExportFormat::Csv => "FORMAT csv, HEADER true",
            ExportFormat::Tsv => "FORMAT csv, HEADER true, DELIMITER '\t'",
            ExportFormat::JsonLines => "FORMAT json",
            ExportFormat::Json => "FORMAT json, ARRAY true",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ExportOutcome {
    pub rows: u64,
    pub path: String,
    pub millis: u64,
}

/// Write the view's rows (optionally only some columns, in the given order) to `path`.
pub fn export_view(
    view: &View,
    columns: Option<Vec<usize>>,
    path: String,
    format: ExportFormat,
) -> Job<ExportOutcome> {
    let view = view.clone();
    let engine = view.dataset.engine.clone();
    let engine_for_job = engine.clone();
    engine.run(Lane::Task, move |conn| {
        let started = Instant::now();
        view.dataset.prepare_remote(conn)?;
        if crate::s3::is_remote(&path) {
            engine_for_job.inner.s3.prepare_duckdb(&engine_for_job, conn, &path)?;
        }
        let inner = view.select_sql();
        let select = match &columns {
            Some(cols) if !cols.is_empty() => {
                let names: Vec<String> = cols
                    .iter()
                    .filter_map(|&ix| view.dataset.columns.get(ix))
                    .map(|c| ident(&c.name))
                    .collect();
                format!("SELECT {} FROM ({inner})", names.join(", "))
            }
            _ => inner,
        };
        let sql = format!(
            "COPY ({select}) TO {} ({})",
            literal(&path),
            format.copy_options()
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.execute([])?;
        Ok(ExportOutcome {
            rows: rows as u64,
            path,
            millis: started.elapsed().as_millis() as u64,
        })
    })
}

/// How copied cells are formatted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CopyFormat {
    Tsv,
    Csv,
    Markdown,
    Json,
    SqlInList,
}

impl CopyFormat {
    pub fn label(self) -> &'static str {
        match self {
            CopyFormat::Tsv => "Tab-separated",
            CopyFormat::Csv => "CSV",
            CopyFormat::Markdown => "Markdown table",
            CopyFormat::Json => "JSON",
            CopyFormat::SqlInList => "SQL IN list",
        }
    }
}

/// Format a block of values as text. `NULL` becomes empty (TSV/CSV) or `NULL` (Markdown).
pub fn format_cells(
    headers: Option<&[String]>,
    rows: &[Vec<Option<String>>],
    format: CopyFormat,
) -> Result<String> {
    match format {
        CopyFormat::Tsv => {
            let mut out = String::new();
            if let Some(headers) = headers {
                out.push_str(&headers.iter().map(|h| tsv_field(h)).collect::<Vec<_>>().join("\t"));
                out.push('\n');
            }
            for row in rows {
                let fields: Vec<String> = row
                    .iter()
                    .map(|v| v.as_deref().map(tsv_field).unwrap_or_default())
                    .collect();
                out.push_str(&fields.join("\t"));
                out.push('\n');
            }
            trim_final_newline(&mut out, rows.len() + headers.map_or(0, |_| 1));
            Ok(out)
        }
        CopyFormat::Csv => {
            let mut out = String::new();
            if let Some(headers) = headers {
                out.push_str(&headers.iter().map(|h| csv_field(h)).collect::<Vec<_>>().join(","));
                out.push('\n');
            }
            for row in rows {
                let fields: Vec<String> = row
                    .iter()
                    .map(|v| v.as_deref().map(csv_field).unwrap_or_default())
                    .collect();
                out.push_str(&fields.join(","));
                out.push('\n');
            }
            Ok(out)
        }
        CopyFormat::Markdown => {
            let width = headers
                .map(|h| h.len())
                .or_else(|| rows.first().map(|r| r.len()))
                .unwrap_or(0);
            let default_headers: Vec<String> = (1..=width).map(|i| format!("column_{i}")).collect();
            let headers = headers.unwrap_or(&default_headers);
            let mut out = String::new();
            out.push_str(&format!(
                "| {} |\n",
                headers.iter().map(|h| markdown_field(h)).collect::<Vec<_>>().join(" | ")
            ));
            out.push_str(&format!(
                "| {} |\n",
                headers.iter().map(|_| "---").collect::<Vec<_>>().join(" | ")
            ));
            for row in rows {
                let fields: Vec<String> = row
                    .iter()
                    .map(|v| v.as_deref().map(markdown_field).unwrap_or_else(|| "NULL".into()))
                    .collect();
                out.push_str(&format!("| {} |\n", fields.join(" | ")));
            }
            Ok(out)
        }
        CopyFormat::SqlInList => {
            let values: Vec<String> = rows
                .iter()
                .flat_map(|row| row.iter())
                .map(|v| match v {
                    Some(v) if v.parse::<f64>().is_ok() => v.clone(),
                    Some(v) => literal(v),
                    None => "NULL".into(),
                })
                .collect();
            Ok(format!("({})", values.join(", ")))
        }
        CopyFormat::Json => Err(Error::other("JSON is produced from typed values; use View::fetch_json")),
    }
}

fn trim_final_newline(out: &mut String, lines: usize) {
    // A single copied value shouldn't carry a trailing newline into a text field.
    if lines == 1 && out.ends_with('\n') {
        out.pop();
    }
}

fn tsv_field(value: &str) -> String {
    if value.contains(['\t', '\n', '\r']) {
        value.replace('\t', " ").replace(['\n', '\r'], " ")
    } else {
        value.to_string()
    }
}

fn csv_field(value: &str) -> String {
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

fn markdown_field(value: &str) -> String {
    value.replace('|', "\\|").replace(['\n', '\r'], " ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows() -> Vec<Vec<Option<String>>> {
        vec![
            vec![Some("1".into()), Some("a,b".into())],
            vec![None, Some("x\"y".into())],
        ]
    }

    #[test]
    fn tsv() {
        let h = vec!["id".to_string(), "name".to_string()];
        assert_eq!(
            format_cells(Some(&h), &rows(), CopyFormat::Tsv).unwrap(),
            "id\tname\n1\ta,b\n\tx\"y\n"
        );
        assert_eq!(
            format_cells(None, &[vec![Some("v".into())]], CopyFormat::Tsv).unwrap(),
            "v"
        );
    }

    #[test]
    fn csv() {
        assert_eq!(
            format_cells(None, &rows(), CopyFormat::Csv).unwrap(),
            "1,\"a,b\"\n,\"x\"\"y\"\n"
        );
    }

    #[test]
    fn markdown() {
        let h = vec!["id".to_string(), "name".to_string()];
        assert_eq!(
            format_cells(Some(&h), &rows(), CopyFormat::Markdown).unwrap(),
            "| id | name |\n| --- | --- |\n| 1 | a,b |\n| NULL | x\"y |\n"
        );
    }

    #[test]
    fn in_list() {
        assert_eq!(
            format_cells(None, &rows(), CopyFormat::SqlInList).unwrap(),
            "(1, 'a,b', NULL, 'x\"y')"
        );
    }
}
