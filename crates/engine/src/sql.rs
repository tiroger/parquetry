//! Small helpers for building SQL text safely.

/// Quote an identifier (column, table, view) for DuckDB.
pub fn ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// Quote a string literal for DuckDB.
pub fn literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// A DuckDB list literal of string literals: `['a', 'b']`.
pub fn literal_list<S: AsRef<str>>(values: &[S]) -> String {
    let items: Vec<String> = values.iter().map(|v| literal(v.as_ref())).collect();
    format!("[{}]", items.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoting() {
        assert_eq!(ident("a\"b"), "\"a\"\"b\"");
        assert_eq!(literal("it's"), "'it''s'");
        assert_eq!(literal_list(&["a", "b'"]), "['a', 'b''']");
    }
}
