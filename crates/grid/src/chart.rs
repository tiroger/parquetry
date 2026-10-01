//! Mini charts for column headers: histograms and top-value bars.

use gpui_kit::*;
use parquetry_engine::{ChartKind, ColumnKind, ColumnSummary, format_epoch_micros, format_number};

/// Colors for a chart, resolved from the theme by the caller.
#[derive(Clone, Copy)]
pub struct ChartColors {
    pub bar: Hsla,
    pub bar_hover: Hsla,
    pub other: Hsla,
    pub null: Hsla,
    pub baseline: Hsla,
}

/// The colour of a kind of data: numbers, text, dates and times, booleans; muted
/// for the rest. Theme charts 1–3 and 5 (4 is nulls).
pub fn kind_color(kind: ColumnKind, theme: &gpui_kit::component::Theme) -> Hsla {
    match kind {
        ColumnKind::Integer | ColumnKind::Float | ColumnKind::Decimal => theme.chart_1,
        ColumnKind::String | ColumnKind::Uuid => theme.chart_2,
        ColumnKind::Date | ColumnKind::Timestamp | ColumnKind::Time | ColumnKind::Interval => theme.chart_3,
        ColumnKind::Boolean => theme.chart_5,
        ColumnKind::Binary | ColumnKind::List | ColumnKind::Struct | ColumnKind::Map | ColumnKind::Other => {
            theme.muted_foreground
        }
    }
}

/// The colour for nulls (bars, the null-share line and labels).
pub fn null_color(theme: &gpui_kit::component::Theme) -> Hsla {
    theme.chart_4
}

/// A short badge for a kind of data, shown before column names.
pub fn kind_badge(kind: ColumnKind) -> &'static str {
    match kind {
        ColumnKind::Integer | ColumnKind::Float | ColumnKind::Decimal => "#",
        ColumnKind::String => "Aa",
        ColumnKind::Uuid => "id",
        ColumnKind::Date => "Dt",
        ColumnKind::Timestamp => "Ts",
        ColumnKind::Time => "Tm",
        ColumnKind::Interval => "Δt",
        ColumnKind::Boolean => "TF",
        ColumnKind::Binary => "01",
        ColumnKind::List => "[]",
        ColumnKind::Struct | ColumnKind::Map => "{}",
        ColumnKind::Other => "?",
    }
}

/// Chart colours for a column of `kind`.
pub fn chart_colors(kind: ColumnKind, theme: &gpui_kit::component::Theme) -> ChartColors {
    let color = kind_color(kind, theme);
    ChartColors {
        bar: color.opacity(0.85),
        bar_hover: color,
        other: theme.muted_foreground.opacity(0.35),
        null: null_color(theme).opacity(0.7),
        baseline: theme.border,
    }
}

/// Paint the chart for `summary` into `bounds`. `hovered` is the bar under the pointer.
pub fn paint_chart(
    summary: &ColumnSummary,
    bounds: Bounds<Pixels>,
    hovered: Option<usize>,
    colors: ChartColors,
    window: &mut Window,
) {
    let width = f32::from(bounds.size.width);
    let height = f32::from(bounds.size.height);
    if width < 4.0 || height < 4.0 {
        return;
    }
    let x0 = f32::from(bounds.origin.x);
    let y0 = f32::from(bounds.origin.y);
    let baseline_y = y0 + height;
    window.paint_quad(fill(
        Bounds::new(point(px(x0), px(baseline_y - 1.0)), size(px(width), px(1.0))),
        colors.baseline,
    ));
    match summary.preferred_chart() {
        ChartKind::Histogram => {
            let bins = &summary.histogram;
            let max = bins.iter().map(|b| b.count).max().unwrap_or(0).max(1) as f32;
            let slot = width / bins.len() as f32;
            let gap = if slot > 4.0 { 1.0 } else { 0.0 };
            for (ix, bin) in bins.iter().enumerate() {
                if bin.count == 0 {
                    continue;
                }
                // Small counts stay visible: at least 1.5px tall.
                let h = ((bin.count as f32 / max) * (height - 2.0)).max(1.5);
                let x = x0 + slot * ix as f32;
                let color = if hovered == Some(ix) { colors.bar_hover } else { colors.bar };
                window.paint_quad(fill(
                    Bounds::new(
                        point(px(x.round()), px(baseline_y - 1.0 - h)),
                        size(px((slot - gap).max(1.0)), px(h)),
                    ),
                    color,
                ));
            }
        }
        ChartKind::TopValues => {
            let bars = top_value_bars(summary);
            let max = bars.iter().map(|b| b.count).max().unwrap_or(0).max(1) as f32;
            let slot = width / bars.len().max(1) as f32;
            let gap = if slot > 6.0 { 2.0 } else { 1.0 };
            for (ix, bar) in bars.iter().enumerate() {
                let h = ((bar.count as f32 / max) * (height - 2.0)).max(1.5);
                let x = x0 + slot * ix as f32;
                let color = match bar.kind {
                    BarKind::Value if hovered == Some(ix) => colors.bar_hover,
                    BarKind::Value => colors.bar,
                    BarKind::Null => colors.null,
                    BarKind::Other => colors.other,
                };
                window.paint_quad(fill(
                    Bounds::new(
                        point(px(x.round()), px(baseline_y - 1.0 - h)),
                        size(px((slot - gap).max(1.0)), px(h)),
                    ),
                    color,
                ));
            }
        }
        ChartKind::None => {}
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BarKind {
    Value,
    Null,
    Other,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Bar {
    pub label: String,
    pub count: u64,
    pub kind: BarKind,
}

/// Bars for a categorical chart: top values, then "other" for the remainder.
pub fn top_value_bars(summary: &ColumnSummary) -> Vec<Bar> {
    let mut bars: Vec<Bar> = summary
        .top_values
        .iter()
        .map(|v| match &v.value {
            Some(value) => Bar {
                label: value.clone(),
                count: v.count,
                kind: BarKind::Value,
            },
            None => Bar {
                label: "null".into(),
                count: v.count,
                kind: BarKind::Null,
            },
        })
        .collect();
    let shown: u64 = bars.iter().map(|b| b.count).sum();
    let other = summary.scanned_rows.saturating_sub(shown);
    if other > 0 {
        bars.push(Bar {
            label: "other values".into(),
            count: other,
            kind: BarKind::Other,
        });
    }
    bars
}

/// Which top-value bar is at a horizontal fraction of the chart.
pub fn top_value_at(summary: &ColumnSummary, fraction: f32) -> Option<usize> {
    let bars = top_value_bars(summary);
    if bars.is_empty() {
        return None;
    }
    Some(((fraction * bars.len() as f32) as usize).min(bars.len() - 1))
}

/// Text for the two ends of the chart's axis, or the categorical summary.
pub fn footer_labels(summary: &ColumnSummary) -> (String, String) {
    match summary.preferred_chart() {
        ChartKind::Histogram => {
            let first = summary.histogram.first().map(|b| b.start);
            let last = summary.histogram.last().map(|b| b.end);
            (
                first.map(|v| axis_label(summary, v)).unwrap_or_default(),
                last.map(|v| axis_label(summary, v)).unwrap_or_default(),
            )
        }
        ChartKind::TopValues => {
            let distinct = summary
                .distinct
                .map(|d| format!("{} distinct", compact_count(d)))
                .unwrap_or_default();
            (distinct, String::new())
        }
        ChartKind::None => match summary.text_length {
            Some((min, _, max)) if min == max => (format!("{min} chars"), String::new()),
            Some((min, _, max)) => (format!("{min}–{max} chars"), String::new()),
            None => (String::new(), String::new()),
        },
    }
}

/// Text shown in place of a chart when bars wouldn't help.
pub fn chart_placeholder(summary: &ColumnSummary) -> String {
    let non_null = summary.scanned_rows.saturating_sub(summary.scanned_nulls);
    if summary.scanned_rows > 0 && non_null == 0 {
        "all null".into()
    } else if summary.is_unique() {
        "all unique".into()
    } else if let Some(distinct) = summary.distinct {
        format!("{} distinct", compact_count(distinct))
    } else {
        String::new()
    }
}

/// Label for an axis value: dates for temporal columns, compact numbers otherwise.
pub fn axis_label(summary: &ColumnSummary, value: f64) -> String {
    if summary.histogram_is_time {
        format_epoch_micros(value, summary.kind == ColumnKind::Date || span_is_days(summary))
    } else if summary.kind == ColumnKind::Integer {
        compact_number(value.round())
    } else {
        compact_number(value)
    }
}

fn span_is_days(summary: &ColumnSummary) -> bool {
    match (summary.histogram.first(), summary.histogram.last()) {
        (Some(a), Some(b)) => (b.end - a.start) > 86_400_000_000.0 * 60.0,
        _ => false,
    }
}

/// `1234567` → `1.23M`; small numbers use [`format_number`].
pub fn compact_number(value: f64) -> String {
    let abs = value.abs();
    let (scaled, suffix) = if abs >= 1e12 {
        (value / 1e12, "T")
    } else if abs >= 1e9 {
        (value / 1e9, "B")
    } else if abs >= 1e6 {
        (value / 1e6, "M")
    } else if abs >= 1e4 {
        (value / 1e3, "K")
    } else {
        return format_number(value);
    };
    let text = format!("{scaled:.2}");
    let text = text.trim_end_matches('0').trim_end_matches('.');
    format!("{text}{suffix}")
}

pub fn compact_count(value: u64) -> String {
    compact_number(value as f64)
}

/// `12345678` → `12,345,678`.
pub fn thousands(value: u64) -> String {
    let digits = value.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

/// Null share as a short label: `""` when none, `"<0.1% null"`, `"12% null"`.
pub fn null_label(summary: &ColumnSummary) -> String {
    let fraction = summary.null_fraction();
    if fraction <= 0.0 {
        return String::new();
    }
    let percent = fraction * 100.0;
    if percent < 0.1 {
        "<0.1% null".into()
    } else if percent < 9.95 {
        format!("{percent:.1}% null")
    } else {
        format!("{percent:.0}% null")
    }
}

#[cfg(test)]
mod tests {
    use super::{compact_number, thousands};

    #[test]
    fn number_labels() {
        assert_eq!(compact_number(999.0), "999");
        assert_eq!(compact_number(12_345.0), "12.35K");
        assert_eq!(compact_number(1_500_000.0), "1.5M");
        assert_eq!(compact_number(-2_000_000_000.0), "-2B");
        assert_eq!(thousands(0), "0");
        assert_eq!(thousands(1234), "1,234");
        assert_eq!(thousands(600_000_000), "600,000,000");
    }
}
