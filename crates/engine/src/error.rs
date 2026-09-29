use std::fmt;

/// Errors surfaced to the UI. Messages are written for people, not for logs.
#[derive(Debug, Clone)]
pub enum Error {
    /// The job was cancelled before it produced a result.
    Cancelled,
    /// A DuckDB error, cleaned up for display.
    Query(String),
    /// Anything else: IO, S3, unsupported input.
    Other(String),
}

impl Error {
    pub fn other(message: impl Into<String>) -> Self {
        Error::Other(message.into())
    }

    pub fn is_cancelled(&self) -> bool {
        matches!(self, Error::Cancelled)
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Cancelled => write!(f, "Cancelled"),
            Error::Query(message) | Error::Other(message) => write!(f, "{message}"),
        }
    }
}

impl std::error::Error for Error {}

impl From<duckdb::Error> for Error {
    fn from(error: duckdb::Error) -> Self {
        let text = error.to_string();
        if text.contains("INTERRUPT") || text.contains("Interrupted") {
            return Error::Cancelled;
        }
        Error::Query(clean_duckdb_message(&text))
    }
}

impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Error::Other(error.to_string())
    }
}

impl From<anyhow::Error> for Error {
    fn from(error: anyhow::Error) -> Self {
        Error::Other(format!("{error:#}"))
    }
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

/// DuckDB messages often carry a long "LINE 1: ..." echo of generated SQL that
/// means nothing to a person looking at a file. Keep the first informative line.
fn clean_duckdb_message(text: &str) -> String {
    let first = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or(text);
    first.to_string()
}
