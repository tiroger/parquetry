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
        assert_eq!(duration_ms(850), "850 ms");
        assert_eq!(duration_ms(2400), "2.4 s");
        assert_eq!(duration_ms(185_000), "3 min 5 s");
    }
}
