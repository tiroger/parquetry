//! Column descriptions and type classification.

use serde::{Deserialize, Serialize};

/// Broad family of a column's type, used to pick summaries, alignment and filters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ColumnKind {
    Integer,
    Float,
    Decimal,
    Boolean,
    String,
    Date,
    Timestamp,
    Time,
    Interval,
    Binary,
    Uuid,
    List,
    Struct,
    Map,
    Other,
}

impl ColumnKind {
    /// Parse a DuckDB type name such as `BIGINT`, `DECIMAL(10,2)` or `STRUCT(a INTEGER)`.
    pub fn from_sql_type(sql_type: &str) -> Self {
        let t = sql_type.trim().to_ascii_uppercase();
        if t.starts_with("STRUCT") {
            return ColumnKind::Struct;
        }
        if t.starts_with("MAP") {
            return ColumnKind::Map;
        }
        if t.ends_with(']') {
            return ColumnKind::List;
        }
        if t.starts_with("UNION") {
            return ColumnKind::Other;
        }
        if t.starts_with("DECIMAL") || t.starts_with("NUMERIC") {
            return ColumnKind::Decimal;
        }
        if t.starts_with("ENUM") {
            return ColumnKind::String;
        }
        if t.starts_with("TIMESTAMP") || t == "DATETIME" {
            return ColumnKind::Timestamp;
        }
        if t.starts_with("TIME") {
            return ColumnKind::Time;
        }
        match t.as_str() {
            "TINYINT" | "SMALLINT" | "INTEGER" | "BIGINT" | "HUGEINT" | "UTINYINT"
            | "USMALLINT" | "UINTEGER" | "UBIGINT" | "UHUGEINT" | "INT" | "INT1" | "INT2"
            | "INT4" | "INT8" => ColumnKind::Integer,
            "FLOAT" | "DOUBLE" | "REAL" | "FLOAT4" | "FLOAT8" => ColumnKind::Float,
            "BOOLEAN" | "BOOL" => ColumnKind::Boolean,
            "VARCHAR" | "JSON" | "TEXT" | "STRING" => ColumnKind::String,
            "DATE" => ColumnKind::Date,
            "INTERVAL" => ColumnKind::Interval,
            "BLOB" | "BYTEA" | "BINARY" | "VARBINARY" => ColumnKind::Binary,
            "UUID" => ColumnKind::Uuid,
            _ => ColumnKind::Other,
        }
    }

    pub fn is_numeric(self) -> bool {
        matches!(
            self,
            ColumnKind::Integer | ColumnKind::Float | ColumnKind::Decimal
        )
    }

    pub fn is_temporal(self) -> bool {
        matches!(self, ColumnKind::Date | ColumnKind::Timestamp)
    }

    pub fn is_nested(self) -> bool {
        matches!(self, ColumnKind::List | ColumnKind::Struct | ColumnKind::Map)
    }

    /// Whether ordering comparisons (`<`, `>`, sort) are meaningful.
    pub fn is_ordered(self) -> bool {
        self.is_numeric()
            || self.is_temporal()
            || matches!(
                self,
                ColumnKind::String | ColumnKind::Time | ColumnKind::Interval | ColumnKind::Uuid
                    | ColumnKind::Boolean
            )
    }

    /// Numbers read better right-aligned.
    pub fn is_right_aligned(self) -> bool {
        self.is_numeric()
    }
}

/// A column of a dataset as DuckDB sees it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ColumnInfo {
    pub name: String,
    /// DuckDB type, e.g. `BIGINT` or `STRUCT(a INTEGER, b VARCHAR)`.
    pub sql_type: String,
    pub kind: ColumnKind,
    /// Parquet physical type when the source is Parquet (e.g. `INT64`, `BYTE_ARRAY`).
    pub physical_type: Option<String>,
    /// Parquet logical/converted type when present (e.g. `StringType()`).
    pub logical_type: Option<String>,
}

impl ColumnInfo {
    pub fn new(name: impl Into<String>, sql_type: impl Into<String>) -> Self {
        let sql_type = sql_type.into();
        Self {
            name: name.into(),
            kind: ColumnKind::from_sql_type(&sql_type),
            sql_type,
            physical_type: None,
            logical_type: None,
        }
    }

    /// A compact, human type label for column headers.
    pub fn type_label(&self) -> String {
        short_type_label(&self.sql_type)
    }
}

/// Map DuckDB type names to the compact labels people know from Arrow/pandas.
pub fn short_type_label(sql_type: &str) -> String {
    let t = sql_type.trim();
    let upper = t.to_ascii_uppercase();
    if let Some(inner) = upper.strip_suffix("[]") {
        let inner_label = short_type_label(&t[..inner.len()]);
        return format!("list<{inner_label}>");
    }
    if upper.ends_with(']')
        && let Some(open) = t.rfind('[') {
            let inner_label = short_type_label(&t[..open]);
            let size = &t[open + 1..t.len() - 1];
            return format!("array<{inner_label}, {size}>");
        }
    if upper.starts_with("STRUCT") {
        return "struct".into();
    }
    if upper.starts_with("MAP") {
        return "map".into();
    }
    if upper.starts_with("UNION") {
        return "union".into();
    }
    if upper.starts_with("ENUM") {
        return "enum".into();
    }
    if upper.starts_with("DECIMAL") || upper.starts_with("NUMERIC") {
        return t.to_ascii_lowercase().replace(' ', "");
    }
    let label = match upper.as_str() {
        "TINYINT" => "int8",
        "SMALLINT" => "int16",
        "INTEGER" => "int32",
        "BIGINT" => "int64",
        "HUGEINT" => "int128",
        "UTINYINT" => "uint8",
        "USMALLINT" => "uint16",
        "UINTEGER" => "uint32",
        "UBIGINT" => "uint64",
        "UHUGEINT" => "uint128",
        "FLOAT" => "float32",
        "DOUBLE" => "float64",
        "BOOLEAN" => "bool",
        "VARCHAR" => "string",
        "JSON" => "json",
        "DATE" => "date",
        "TIME" => "time",
        "TIME WITH TIME ZONE" => "timetz",
        "TIMESTAMP" => "timestamp",
        "TIMESTAMP_S" => "timestamp[s]",
        "TIMESTAMP_MS" => "timestamp[ms]",
        "TIMESTAMP_NS" => "timestamp[ns]",
        "TIMESTAMP WITH TIME ZONE" => "timestamptz",
        "INTERVAL" => "interval",
        "BLOB" => "binary",
        "UUID" => "uuid",
        "BIT" => "bit",
        _ => return t.to_ascii_lowercase(),
    };
    label.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify() {
        assert_eq!(ColumnKind::from_sql_type("BIGINT"), ColumnKind::Integer);
        assert_eq!(ColumnKind::from_sql_type("DECIMAL(10,2)"), ColumnKind::Decimal);
        assert_eq!(ColumnKind::from_sql_type("BIGINT[]"), ColumnKind::List);
        assert_eq!(ColumnKind::from_sql_type("INTEGER[3]"), ColumnKind::List);
        assert_eq!(
            ColumnKind::from_sql_type("STRUCT(a BIGINT, b VARCHAR[])"),
            ColumnKind::Struct
        );
        assert_eq!(
            ColumnKind::from_sql_type("MAP(VARCHAR, INTEGER)"),
            ColumnKind::Map
        );
        assert_eq!(
            ColumnKind::from_sql_type("TIMESTAMP WITH TIME ZONE"),
            ColumnKind::Timestamp
        );
        assert_eq!(ColumnKind::from_sql_type("TIME"), ColumnKind::Time);
        assert_eq!(ColumnKind::from_sql_type("ENUM('a', 'b')"), ColumnKind::String);
    }

    #[test]
    fn labels() {
        assert_eq!(short_type_label("BIGINT"), "int64");
        assert_eq!(short_type_label("BIGINT[]"), "list<int64>");
        assert_eq!(short_type_label("VARCHAR[][]"), "list<list<string>>");
        assert_eq!(short_type_label("DECIMAL(10, 2)"), "decimal(10,2)");
        assert_eq!(short_type_label("INTEGER[3]"), "array<int32, 3>");
        assert_eq!(short_type_label("STRUCT(a INTEGER)"), "struct");
    }
}
