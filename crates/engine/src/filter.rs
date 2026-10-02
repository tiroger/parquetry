//! Column filters, sorting and search, and their SQL.

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::sql::{ident, literal};
use crate::types::{ColumnInfo, ColumnKind};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FilterOp {
    Equals,
    NotEquals,
    Less,
    LessOrEqual,
    Greater,
    GreaterOrEqual,
    Between,
    In,
    NotIn,
    Contains,
    NotContains,
    StartsWith,
    EndsWith,
    Matches,
    IsNull,
    IsNotNull,
    IsTrue,
    IsFalse,
    IsEmpty,
    IsNotEmpty,
}

impl FilterOp {
    pub fn label(self) -> &'static str {
        match self {
            FilterOp::Equals => "equals",
            FilterOp::NotEquals => "does not equal",
            FilterOp::Less => "less than",
            FilterOp::LessOrEqual => "at most",
            FilterOp::Greater => "greater than",
            FilterOp::GreaterOrEqual => "at least",
            FilterOp::Between => "between",
            FilterOp::In => "is one of",
            FilterOp::NotIn => "is not one of",
            FilterOp::Contains => "contains",
            FilterOp::NotContains => "does not contain",
            FilterOp::StartsWith => "starts with",
            FilterOp::EndsWith => "ends with",
            FilterOp::Matches => "matches regex",
            FilterOp::IsNull => "is null",
            FilterOp::IsNotNull => "is not null",
            FilterOp::IsTrue => "is true",
            FilterOp::IsFalse => "is false",
            FilterOp::IsEmpty => "is empty",
            FilterOp::IsNotEmpty => "is not empty",
        }
    }

    /// Compact symbol for filter chips.
    pub fn symbol(self) -> &'static str {
        match self {
            FilterOp::Equals => "=",
            FilterOp::NotEquals => "≠",
            FilterOp::Less => "<",
            FilterOp::LessOrEqual => "≤",
            FilterOp::Greater => ">",
            FilterOp::GreaterOrEqual => "≥",
            other => other.label(),
        }
    }

    /// How many values the operator takes: 0, 1, or 2 (between).
    pub fn arity(self) -> usize {
        match self {
            FilterOp::IsNull
            | FilterOp::IsNotNull
            | FilterOp::IsTrue
            | FilterOp::IsFalse
            | FilterOp::IsEmpty
            | FilterOp::IsNotEmpty => 0,
            FilterOp::Between => 2,
            _ => 1,
        }
    }

    /// Operators offered for a column kind, most useful first.
    pub fn for_kind(kind: ColumnKind) -> Vec<FilterOp> {
        use FilterOp::*;
        let mut ops = match kind {
            ColumnKind::Integer | ColumnKind::Float | ColumnKind::Decimal => vec![
                Equals, NotEquals, Greater, GreaterOrEqual, Less, LessOrEqual, Between, In, NotIn,
            ],
            ColumnKind::Date | ColumnKind::Timestamp | ColumnKind::Time | ColumnKind::Interval => {
                vec![
                    Between, Equals, NotEquals, Greater, GreaterOrEqual, Less, LessOrEqual,
                ]
            }
            ColumnKind::Boolean => vec![IsTrue, IsFalse],
            ColumnKind::String | ColumnKind::Uuid => vec![
                Contains, Equals, NotEquals, StartsWith, EndsWith, Matches, In, NotIn,
                NotContains, IsEmpty, IsNotEmpty, Greater, Less,
            ],
            ColumnKind::List | ColumnKind::Map => {
                vec![Contains, NotContains, IsEmpty, IsNotEmpty, Equals, Matches]
            }
            ColumnKind::Struct | ColumnKind::Binary | ColumnKind::Other => {
                vec![Contains, NotContains, Equals, NotEquals, Matches]
            }
        };
        ops.push(IsNull);
        ops.push(IsNotNull);
        ops
    }
}

/// A condition on one column.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Filter {
    pub column: String,
    pub op: FilterOp,
    pub value: String,
    /// Upper bound for `Between`.
    pub value2: String,
}

impl Filter {
    pub fn new(column: impl Into<String>, op: FilterOp, value: impl Into<String>) -> Self {
        Self {
            column: column.into(),
            op,
            value: value.into(),
            value2: String::new(),
        }
    }

    pub fn between(column: impl Into<String>, low: impl Into<String>, high: impl Into<String>) -> Self {
        Self {
            column: column.into(),
            op: FilterOp::Between,
            value: low.into(),
            value2: high.into(),
        }
    }

    /// Chip text, e.g. `amount > 5000` or `name contains “bob”`.
    pub fn describe(&self) -> String {
        match self.op.arity() {
            0 => format!("{} {}", self.column, self.op.label()),
            2 => format!("{} between {} and {}", self.column, self.value, self.value2),
            _ => {
                let value = match self.op {
                    FilterOp::Contains
                    | FilterOp::NotContains
                    | FilterOp::StartsWith
                    | FilterOp::EndsWith
                    | FilterOp::Matches => format!("“{}”", self.value),
                    _ => self.value.clone(),
                };
                format!("{} {} {}", self.column, self.op.symbol(), value)
            }
        }
    }

    /// SQL predicate. `alias` qualifies the column (e.g. `s`).
    pub fn to_sql(&self, column: &ColumnInfo, alias: Option<&str>) -> Result<String> {
        let col = qualified(alias, &column.name);
        let as_text = format!("CAST({col} AS VARCHAR)");
        let typed = |value: &str| -> String {
            if column.kind.is_nested() || matches!(column.kind, ColumnKind::Other | ColumnKind::Binary) {
                literal(value)
            } else if column.sql_type.eq_ignore_ascii_case("VARCHAR") {
                // Already text; keeps generated code readable.
                literal(value.trim())
            } else {
                format!("CAST({} AS {})", literal(value.trim()), column.sql_type)
            }
        };
        let lhs = if column.kind.is_nested() || matches!(column.kind, ColumnKind::Other | ColumnKind::Binary) {
            as_text.clone()
        } else {
            col.clone()
        };
        let list = |value: &str| -> Result<String> {
            let items: Vec<String> = value
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(typed)
                .collect();
            if items.is_empty() {
                return Err(Error::other(format!("Enter values for {} separated by commas", column.name)));
            }
            Ok(items.join(", "))
        };
        let needs_value = |value: &str| -> Result<()> {
            if value.trim().is_empty() && !matches!(column.kind, ColumnKind::String) {
                Err(Error::other(format!("Enter a value for {}", column.name)))
            } else {
                Ok(())
            }
        };
        let sql = match self.op {
            FilterOp::Equals => {
                needs_value(&self.value)?;
                format!("{lhs} = {}", typed(&self.value))
            }
            FilterOp::NotEquals => {
                needs_value(&self.value)?;
                format!("{lhs} IS DISTINCT FROM {}", typed(&self.value))
            }
            FilterOp::Less => {
                needs_value(&self.value)?;
                format!("{lhs} < {}", typed(&self.value))
            }
            FilterOp::LessOrEqual => {
                needs_value(&self.value)?;
                format!("{lhs} <= {}", typed(&self.value))
            }
            FilterOp::Greater => {
                needs_value(&self.value)?;
                format!("{lhs} > {}", typed(&self.value))
            }
            FilterOp::GreaterOrEqual => {
                needs_value(&self.value)?;
                format!("{lhs} >= {}", typed(&self.value))
            }
            FilterOp::Between => {
                needs_value(&self.value)?;
                needs_value(&self.value2)?;
                format!(
                    "{lhs} BETWEEN {} AND {}",
                    typed(&self.value),
                    typed(&self.value2)
                )
            }
            FilterOp::In => format!("{lhs} IN ({})", list(&self.value)?),
            FilterOp::NotIn => format!("({lhs} NOT IN ({}) OR {col} IS NULL)", list(&self.value)?),
            FilterOp::Contains => format!(
                "contains(lower({as_text}), {})",
                literal(&self.value.to_lowercase())
            ),
            FilterOp::NotContains => format!(
                "(NOT contains(lower({as_text}), {}) OR {col} IS NULL)",
                literal(&self.value.to_lowercase())
            ),
            FilterOp::StartsWith => format!(
                "starts_with(lower({as_text}), {})",
                literal(&self.value.to_lowercase())
            ),
            FilterOp::EndsWith => format!(
                "ends_with(lower({as_text}), {})",
                literal(&self.value.to_lowercase())
            ),
            FilterOp::Matches => format!("regexp_matches({as_text}, {})", literal(&self.value)),
            FilterOp::IsNull => format!("{col} IS NULL"),
            FilterOp::IsNotNull => format!("{col} IS NOT NULL"),
            FilterOp::IsTrue => format!("{col} IS TRUE"),
            FilterOp::IsFalse => format!("{col} IS FALSE"),
            FilterOp::IsEmpty => match column.kind {
                ColumnKind::List => format!("len({col}) = 0"),
                ColumnKind::Map => format!("cardinality({col}) = 0"),
                _ => format!("{as_text} = ''"),
            },
            FilterOp::IsNotEmpty => match column.kind {
                ColumnKind::List => format!("len({col}) > 0"),
                ColumnKind::Map => format!("cardinality({col}) > 0"),
                _ => format!("{as_text} <> ''"),
            },
        };
        Ok(sql)
    }

    /// Literal values this filter casts, so they can be validated before running.
    pub(crate) fn typed_values(&self, column: &ColumnInfo) -> Vec<String> {
        if column.kind.is_nested()
            || matches!(column.kind, ColumnKind::Other | ColumnKind::Binary | ColumnKind::String)
        {
            return Vec::new();
        }
        match self.op {
            FilterOp::Equals
            | FilterOp::NotEquals
            | FilterOp::Less
            | FilterOp::LessOrEqual
            | FilterOp::Greater
            | FilterOp::GreaterOrEqual => vec![self.value.trim().to_string()],
            FilterOp::Between => vec![
                self.value.trim().to_string(),
                self.value2.trim().to_string(),
            ],
            FilterOp::In | FilterOp::NotIn => self
                .value
                .split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect(),
            _ => Vec::new(),
        }
    }
}

/// One sort key.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SortKey {
    pub column: String,
    pub descending: bool,
}

impl SortKey {
    pub fn asc(column: impl Into<String>) -> Self {
        Self {
            column: column.into(),
            descending: false,
        }
    }

    pub fn desc(column: impl Into<String>) -> Self {
        Self {
            column: column.into(),
            descending: true,
        }
    }
}

/// Everything that turns a dataset into what's on screen: filters, search and sort.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ViewSpec {
    pub filters: Vec<Filter>,
    /// Case-insensitive text search across all columns.
    pub search: String,
    /// A free-form SQL `WHERE` condition.
    pub where_sql: String,
    pub sort: Vec<SortKey>,
}

impl ViewSpec {
    pub fn is_identity(&self) -> bool {
        self.filters.is_empty()
            && self.search.trim().is_empty()
            && self.where_sql.trim().is_empty()
            && self.sort.is_empty()
    }

    pub fn has_filter(&self) -> bool {
        !self.filters.is_empty() || !self.search.trim().is_empty() || !self.where_sql.trim().is_empty()
    }

    /// The combined `WHERE` condition, or `None` when nothing filters.
    pub fn where_clause(&self, columns: &[ColumnInfo], alias: Option<&str>) -> Result<Option<String>> {
        let mut parts = Vec::new();
        for filter in &self.filters {
            let column = find_column(columns, &filter.column)?;
            parts.push(filter.to_sql(column, alias)?);
        }
        let search = self.search.trim();
        if !search.is_empty() {
            let needle = literal(&search.to_lowercase());
            let ors: Vec<String> = columns
                .iter()
                .map(|c| {
                    format!(
                        "contains(lower(CAST({} AS VARCHAR)), {needle})",
                        qualified(alias, &c.name)
                    )
                })
                .collect();
            if !ors.is_empty() {
                parts.push(format!("({})", ors.join(" OR ")));
            }
        }
        let custom = self.where_sql.trim();
        if !custom.is_empty() {
            parts.push(format!("({custom})"));
        }
        if parts.is_empty() {
            Ok(None)
        } else {
            Ok(Some(parts.join(" AND ")))
        }
    }

    /// Sort keys as SQL (without `ORDER BY`), or `None`.
    pub fn order_clause(&self, columns: &[ColumnInfo], alias: Option<&str>) -> Result<Option<String>> {
        if self.sort.is_empty() {
            return Ok(None);
        }
        let mut keys = Vec::new();
        for key in &self.sort {
            let column = find_column(columns, &key.column)?;
            let dir = if key.descending { "DESC" } else { "ASC" };
            keys.push(format!("{} {dir} NULLS LAST", qualified(alias, &column.name)));
        }
        Ok(Some(keys.join(", ")))
    }

    pub fn sort_for(&self, column: &str) -> Option<&SortKey> {
        self.sort.iter().find(|k| k.column == column)
    }
}

fn find_column<'a>(columns: &'a [ColumnInfo], name: &str) -> Result<&'a ColumnInfo> {
    columns
        .iter()
        .find(|c| c.name == name)
        .ok_or_else(|| Error::other(format!("There’s no column named “{name}”")))
}

pub(crate) fn qualified(alias: Option<&str>, name: &str) -> String {
    match alias {
        Some(alias) => format!("{alias}.{}", ident(name)),
        None => ident(name),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cols() -> Vec<ColumnInfo> {
        vec![
            ColumnInfo::new("amount", "DOUBLE"),
            ColumnInfo::new("name", "VARCHAR"),
            ColumnInfo::new("tags", "VARCHAR[]"),
        ]
    }

    #[test]
    fn filter_sql() {
        let c = cols();
        let f = Filter::new("amount", FilterOp::Greater, "5");
        assert_eq!(f.to_sql(&c[0], None).unwrap(), "\"amount\" > CAST('5' AS DOUBLE)");
        let f = Filter::new("name", FilterOp::Contains, "Bo'b");
        assert_eq!(
            f.to_sql(&c[1], Some("s")).unwrap(),
            "contains(lower(CAST(s.\"name\" AS VARCHAR)), 'bo''b')"
        );
        let f = Filter::new("amount", FilterOp::In, "1, 2,,3");
        assert_eq!(
            f.to_sql(&c[0], None).unwrap(),
            "\"amount\" IN (CAST('1' AS DOUBLE), CAST('2' AS DOUBLE), CAST('3' AS DOUBLE))"
        );
        let f = Filter::new("tags", FilterOp::IsEmpty, "");
        assert_eq!(f.to_sql(&c[2], None).unwrap(), "len(\"tags\") = 0");
        assert!(Filter::new("amount", FilterOp::Equals, " ").to_sql(&c[0], None).is_err());
    }

    #[test]
    fn spec_sql() {
        let c = cols();
        let spec = ViewSpec {
            filters: vec![Filter::new("amount", FilterOp::IsNotNull, "")],
            search: "x".into(),
            where_sql: "amount < 3".into(),
            sort: vec![SortKey::desc("amount")],
        };
        let w = spec.where_clause(&c, None).unwrap().unwrap();
        assert!(w.starts_with("\"amount\" IS NOT NULL AND (contains("));
        assert!(w.ends_with("AND (amount < 3)"));
        assert_eq!(
            spec.order_clause(&c, None).unwrap().unwrap(),
            "\"amount\" DESC NULLS LAST"
        );
        assert!(ViewSpec::default().is_identity());
        let bad = ViewSpec {
            sort: vec![SortKey::asc("nope")],
            ..Default::default()
        };
        assert!(bad.order_clause(&c, None).is_err());
    }

    #[test]
    fn describe() {
        assert_eq!(Filter::new("a", FilterOp::Greater, "5").describe(), "a > 5");
        assert_eq!(Filter::new("a", FilterOp::Contains, "x").describe(), "a contains “x”");
        assert_eq!(Filter::between("a", "1", "2").describe(), "a between 1 and 2");
        assert_eq!(Filter::new("a", FilterOp::IsNull, "").describe(), "a is null");
    }
}
