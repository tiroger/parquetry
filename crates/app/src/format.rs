//! Human-friendly text for sizes, counts and durations.

/// `1536` → `1.5 KB` (decimal units, as Finder shows them).
pub fn bytes(value: u64) -> String {
    const UNITS: [&str; 6] = ["bytes", "KB", "MB", "GB", "TB", "PB"];
    if value < 1000 {
        return if value == 1 { "1 byte".into() } else { format!("{value} bytes") };
    }
    let mut size = value as f64;
    let mut unit = 0;
    while size >= 1000.0 && unit < UNITS.len() - 1 {
        size /= 1000.0;
        unit += 1;
    }
    let text = if size >= 100.0 { format!("{size:.0}") } else { format!("{size:.1}") };
    format!("{} {}", text.trim_end_matches(".0"), UNITS[unit])
}

/// `12345678` → `12,345,678`.
pub fn count(value: u64) -> String {
    parquetry_grid::chart::thousands(value)
}

/// `1` → `1 row`, `2` → `2 rows`.
pub fn plural(value: u64, singular: &str, plural: &str) -> String {
    if value == 1 {
        format!("1 {singular}")
    } else {
        format!("{} {plural}", count(value))
    }
}

/// Milliseconds as `850 ms`, `2.4 s` or `3 min 5 s`.
pub fn duration_ms(ms: u64) -> String {
    if ms < 1000 {
        format!("{ms} ms")
    } else if ms < 60_000 {
        format!("{:.1} s", ms as f64 / 1000.0)
    } else {
        format!("{} min {} s", ms / 60_000, (ms % 60_000) / 1000)
    }
}

/// Cut `text` to `max` characters with an ellipsis.
pub fn short(text: &str, max: usize) -> String {
    parquetry_grid::layout::truncate_chars(text, max).unwrap_or_else(|| text.to_string())
}

/// Shorten a path for display: home becomes `~`.
pub fn display_path(path: &str) -> String {
    if let Some(home) = dirs::home_dir() {
        let home = home.to_string_lossy();
        if let Some(rest) = path.strip_prefix(home.as_ref()) {
            return format!("~{rest}");
        }
    }
    path.to_string()
}

/// Split a location into its folder (for display, with `~` for home) and its last
/// part: `/Users/me/data/a.parquet` → (`~/data`, `a.parquet`).
pub fn split_location(location: &str) -> (String, String) {
    let trimmed = location.trim_end_matches(['/', '\\']);
    let remote = parquetry_engine::is_remote(trimmed);
    let cut = if remote { trimmed.rfind('/') } else { trimmed.rfind(['/', '\\']) };
    match cut {
        // Keep `s3://bucket` whole rather than splitting the scheme.
        Some(ix) if !(remote && trimmed[..ix].ends_with('/')) => {
            let (folder, name) = (&trimmed[..ix], &trimmed[ix + 1..]);
            let folder = if folder.is_empty() { "/".to_string() } else { display_path(folder) };
            (folder, name.to_string())
        }
        _ => (String::new(), trimmed.to_string()),
    }
}

/// How long ago `then` was, both in seconds since the Unix epoch: `just now`,
/// `5 min ago`, `3 h ago`, `yesterday`, `4 days ago`, `2 weeks ago`, `5 months ago`.
pub fn ago(then: u64, now: u64) -> String {
    const MIN: u64 = 60;
    const HOUR: u64 = 60 * MIN;
    const DAY: u64 = 24 * HOUR;
    let elapsed = now.saturating_sub(then);
    match elapsed {
        e if e < MIN => "just now".into(),
        e if e < HOUR => format!("{} min ago", e / MIN),
        e if e < DAY => format!("{} h ago", e / HOUR),
        e if e < 2 * DAY => "yesterday".into(),
        e if e < 14 * DAY => format!("{} days ago", e / DAY),
        e if e < 60 * DAY => format!("{} weeks ago", e / (7 * DAY)),
        e if e < 365 * DAY => format!("{} months ago", e / (30 * DAY)),
        e => plural(e / (365 * DAY), "year", "years") + " ago",
    }
}

/// Seconds since the Unix epoch.
pub fn now_secs() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats() {
        assert_eq!(bytes(0), "0 bytes");
        assert_eq!(bytes(1), "1 byte");
        assert_eq!(bytes(1536), "1.5 KB");
        assert_eq!(bytes(1_713_932_119), "1.7 GB");
        assert_eq!(bytes(250_000_000), "250 MB");
        assert_eq!(bytes(2_000_000), "2 MB");
        assert_eq!(plural(1, "row", "rows"), "1 row");
        assert_eq!(plural(1200, "row", "rows"), "1,200 rows");
        assert_eq!(split_location("/data/x/a.parquet"), ("/data/x".to_string(), "a.parquet".to_string()));
        assert_eq!(split_location("/a.parquet"), ("/".to_string(), "a.parquet".to_string()));
        assert_eq!(split_location("s3://bucket/dir/"), ("s3://bucket".to_string(), "dir".to_string()));
        assert_eq!(split_location("s3://bucket"), (String::new(), "s3://bucket".to_string()));
        assert_eq!(split_location(r"C:\data\a.parquet"), (r"C:\data".to_string(), "a.parquet".to_string()));
        assert_eq!(duration_ms(850), "850 ms");
        assert_eq!(duration_ms(2400), "2.4 s");
        assert_eq!(duration_ms(185_000), "3 min 5 s");
        let now = 1_000_000_000;
        assert_eq!(ago(now - 10, now), "just now");
        assert_eq!(ago(now + 10, now), "just now", "clock skew");
        assert_eq!(ago(now - 5 * 60, now), "5 min ago");
        assert_eq!(ago(now - 3 * 3600, now), "3 h ago");
        assert_eq!(ago(now - 30 * 3600, now), "yesterday");
        assert_eq!(ago(now - 4 * 86_400, now), "4 days ago");
        assert_eq!(ago(now - 20 * 86_400, now), "2 weeks ago");
        assert_eq!(ago(now - 150 * 86_400, now), "5 months ago");
        assert_eq!(ago(now - 800 * 86_400, now), "2 years ago");
    }
}
