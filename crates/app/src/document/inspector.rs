//! The inspector panel: the full value of the selected cell and details of its column.

use std::time::Duration;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme as _, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::component::Disableable as _;
use gpui_kit::*;
use parquetry_engine::{ChartKind, ColumnInfo, ColumnSummary, format_number};
use parquetry_grid::chart::{ChartColors, axis_label, chart_placeholder, paint_chart, thousands, top_value_bars};
use parquetry_grid::{GridState, SummaryState};

enum Value {
    None,
    Loading,
    Loaded { text: Option<String>, pretty: Option<String> },
    Failed(String),
}

pub struct Inspector {
    grid: Entity<GridState>,
    /// A column pinned from "Column Details"; otherwise the active cell's column.
    column: Option<usize>,
    cell: Option<(u64, usize)>,
    value: Value,
    task: Option<Task<()>>,
    view_id: Option<u64>,
}

impl Inspector {
    pub fn new(grid: Entity<GridState>, _: &mut Context<Self>) -> Self {
        Self {
            grid,
            column: None,
            cell: None,
            value: Value::None,
            task: None,
            view_id: None,
        }
    }

    /// Show a column's details (until a cell is selected).
    pub fn show_column(&mut self, column: usize, cx: &mut Context<Self>) {
        self.column = Some(column);
        self.grid.update(cx, |g, cx| g.request_summaries(&[column], cx));
        cx.notify();
    }

    /// Follow the grid's active cell.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let grid = self.grid.read(cx);
        let cell = grid.active_cell();
        let view = grid.view().cloned();
        let view_id = view.as_ref().map(|v| v.id);
        if cell == self.cell && view_id == self.view_id {
            return;
        }
        self.cell = cell;
        self.view_id = view_id;
        if cell.is_some() {
            self.column = None;
        }
        let (Some((row, column)), Some(view)) = (cell, view) else {
            self.value = Value::None;
            self.task = None;
            cx.notify();
            return;
        };
        self.value = Value::Loading;
        let nested = view.columns()[column].kind.is_nested();
        self.task = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            // Debounce: arrow-key repeat shouldn't fetch every value along the way.
            cx.background_executor().timer(Duration::from_millis(60)).await;
            let text = view.fetch_values(row..row + 1, vec![column], 1).await;
            let pretty = if nested {
                view.fetch_json(row..row + 1, vec![column], 1).await.ok()
            } else {
                None
            };
            let _ = this.update(cx, |this, cx| {
                this.value = match text {
                    Ok(rows) => {
                        let text = rows.into_iter().next().and_then(|r| r.into_iter().next()).flatten();
                        let pretty = pretty
                            .and_then(|lines| lines.into_iter().next())
                            .and_then(|line| pretty_json_field(&line))
                            .or_else(|| text.as_deref().and_then(pretty_json_text));
                        Value::Loaded { text, pretty }
                    }
                    Err(error) if error.is_cancelled() => return,
                    Err(error) => Value::Failed(error.to_string()),
                };
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn render_value(&self, info: &ColumnInfo, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let (row, _) = self.cell.unwrap_or_default();
        let body: AnyElement = match &self.value {
            Value::None => div().into_any_element(),
            Value::Loading => div().text_color(theme.muted_foreground).child("Loading…").into_any_element(),
            Value::Failed(message) => div().text_color(theme.danger).child(message.clone()).into_any_element(),
            Value::Loaded { text: None, .. } => div()
                .italic()
                .text_color(theme.muted_foreground)
                .child("null")
                .into_any_element(),
            Value::Loaded { text: Some(text), pretty } => {
                let shown = pretty.clone().unwrap_or_else(|| text.clone());
                div()
                    .id("inspector-value")
                    .font_family(theme.mono_font_family.clone())
                    .text_sm()
                    .whitespace_normal()
                    .child(shown)
                    .into_any_element()
            }
        };
        let copy_text = match &self.value {
            Value::Loaded { text: Some(text), pretty } => Some(pretty.clone().unwrap_or_else(|| text.clone())),
            _ => None,
        };
        let length = match &self.value {
            Value::Loaded { text: Some(text), .. } => Some(text.chars().count()),
            _ => None,
        };
        v_flex()
            .gap_2()
            .child(
                h_flex()
                    .justify_between()
                    .child(
                        v_flex()
                            .child(div().text_xs().text_color(theme.muted_foreground).child(format!("Row {}", thousands(row))))
                            .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child(info.name.clone())),
                    )
                    .child(
                        Button::new("copy-value")
                            .icon(IconName::Copy)
                            .label("Copy")
                            .xsmall()
                            .ghost()
                            .disabled(copy_text.is_none())
                            .on_click(move |_, window, cx| {
                                if let Some(text) = &copy_text {
                                    cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                                    use gpui_kit::component::WindowExt as _;
                                    window.push_notification("Value copied", cx);
                                }
                            }),
                    ),
            )
            .child(
                div()
                    .p_2()
                    .rounded(theme.radius)
                    .bg(theme.secondary)
                    .border_1()
                    .border_color(theme.border)
                    .child(body),
            )
            .when_some(length, |this, len| {
                this.child(div().text_xs().text_color(theme.muted_foreground).child(format!("{} characters", thousands(len as u64))))
            })
            .into_any_element()
    }

    fn render_column(&self, column: usize, info: &ColumnInfo, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let summary = match self.grid.read(cx).summary(column) {
            Some(SummaryState::Ready(summary)) => Some(summary.clone()),
            Some(SummaryState::Failed(message)) => {
                return div().text_color(theme.danger).child(message.clone()).into_any_element();
            }
            _ => None,
        };
        let mut facts: Vec<(String, String)> = vec![("Type".into(), info.sql_type.clone())];
        if let Some(physical) = &info.physical_type {
            facts.push(("Parquet type".into(), physical.clone()));
        }
        if let Some(logical) = &info.logical_type {
            facts.push(("Logical type".into(), logical.clone()));
        }
        let mut chart_el: Option<AnyElement> = None;
        let mut bars: Vec<AnyElement> = Vec::new();
        if let Some(s) = &summary {
            facts.extend(stat_rows(s));
            let colors = ChartColors {
                bar: theme.chart_1.opacity(0.85),
                bar_hover: theme.chart_1,
                other: theme.muted_foreground.opacity(0.35),
                null: theme.warning.opacity(0.6),
                baseline: theme.border,
            };
            match s.preferred_chart() {
                ChartKind::Histogram => {
                    let chart_summary = s.clone();
                    let (lo, hi) = (
                        s.histogram.first().map(|b| axis_label(s, b.start)).unwrap_or_default(),
                        s.histogram.last().map(|b| axis_label(s, b.end)).unwrap_or_default(),
                    );
                    chart_el = Some(
                        v_flex()
                            .gap_1()
                            .child(
                                canvas(|_, _, _| (), move |bounds, _, window, _| {
                                    paint_chart(&chart_summary, bounds, None, colors, window)
                                })
                                .w_full()
                                .h(rems(6.)),
                            )
                            .child(
                                h_flex()
                                    .justify_between()
                                    .text_xs()
                                    .font_family(theme.mono_font_family.clone())
                                    .text_color(theme.muted_foreground)
                                    .child(lo)
                                    .child(hi),
                            )
                            .into_any_element(),
                    );
                }
                ChartKind::TopValues => {
                    let total = s.scanned_rows.max(1);
                    let max = s.top_values.iter().map(|v| v.count).max().unwrap_or(1).max(1);
                    for (ix, bar) in top_value_bars(s).into_iter().enumerate() {
                        let fraction = bar.count as f32 / max.max(bar.count) as f32;
                        let color = match bar.kind {
                            parquetry_grid::chart::BarKind::Value => theme.chart_1.opacity(0.8),
                            parquetry_grid::chart::BarKind::Null => theme.warning.opacity(0.6),
                            parquetry_grid::chart::BarKind::Other => theme.muted_foreground.opacity(0.3),
                        };
                        bars.push(
                            v_flex()
                                .id(("top-value", ix))
                                .gap_0p5()
                                .child(
                                    h_flex()
                                        .justify_between()
                                        .gap_2()
                                        .text_xs()
                                        .child(div().truncate().font_family(theme.mono_font_family.clone()).child(bar.label.clone()))
                                        .child(
                                            div()
                                                .flex_shrink_0()
                                                .text_color(theme.muted_foreground)
                                                .child(format!("{} · {:.1}%", thousands(bar.count), bar.count as f64 * 100.0 / total as f64)),
                                        ),
                                )
                                .child(
                                    div()
                                        .h(rems(0.375))
                                        .w_full()
                                        .rounded_full()
                                        .bg(theme.secondary)
                                        .child(div().h_full().rounded_full().bg(color).w(relative(fraction.min(1.0)))),
                                )
                                .into_any_element(),
                        );
                    }
                }
                ChartKind::None => {
                    chart_el = Some(div().text_sm().text_color(theme.muted_foreground).child(chart_placeholder(s)).into_any_element());
                }
            }
        }
        let loading = summary.is_none();
        v_flex()
            .gap_3()
            .child(
                v_flex()
                    .child(div().text_xs().text_color(theme.muted_foreground).child("Column"))
                    .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child(info.name.clone())),
            )
            .children(chart_el)
            .when(!bars.is_empty(), |this| this.child(v_flex().gap_2().children(bars)))
            .child(
                v_flex().gap_1().children(facts.into_iter().map(|(label, value)| {
                    h_flex()
                        .gap_3()
                        .justify_between()
                        .text_xs()
                        .child(div().flex_shrink_0().text_color(theme.muted_foreground).child(label))
                        .child(div().truncate().font_family(theme.mono_font_family.clone()).child(value))
                })),
            )
            .when(loading, |this| this.child(div().text_xs().text_color(theme.muted_foreground).child("Summarizing…")))
            .into_any_element()
    }
}

fn stat_rows(s: &ColumnSummary) -> Vec<(String, String)> {
    let mut rows = vec![("Rows".to_string(), thousands(s.rows))];
    if s.sampled {
        rows.push(("Sample".into(), format!("{} rows", thousands(s.scanned_rows))));
    }
    rows.push((
        "Nulls".into(),
        match s.exact_nulls {
            Some(n) => format!("{} ({:.2}%)", thousands(n), s.null_fraction() * 100.0),
            None => format!("≈ {:.2}%", s.null_fraction() * 100.0),
        },
    ));
    if let Some(d) = s.distinct {
        let prefix = if s.distinct_exact && !s.sampled { "" } else { "≈ " };
        rows.push(("Distinct".into(), format!("{prefix}{}", thousands(d))));
    }
    if let Some(v) = &s.min {
        rows.push(("Min".into(), v.clone()));
    }
    if let Some(v) = &s.max {
        rows.push(("Max".into(), v.clone()));
    }
    if let Some(v) = s.mean {
        rows.push(("Mean".into(), format_number(v)));
    }
    if let Some(v) = s.std_dev {
        rows.push(("Std dev".into(), format_number(v)));
    }
    if let Some([a, b, c]) = &s.quantiles {
        rows.push(("25th percentile".into(), a.clone()));
        rows.push(("Median".into(), b.clone()));
        rows.push(("75th percentile".into(), c.clone()));
    }
    if let Some((min, avg, max)) = s.text_length {
        rows.push(("Length".into(), format!("{min}–{max}, avg {avg:.1}")));
    }
    rows.push(("Computed in".into(), crate::format::duration_ms(s.millis)));
    rows
}

/// `{"col": value}` → pretty-printed `value`.
fn pretty_json_field(line: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    let inner = value.as_object()?.values().next()?.clone();
    if inner.is_object() || inner.is_array() {
        serde_json::to_string_pretty(&inner).ok()
    } else {
        None
    }
}

/// Strings that hold JSON documents are pretty-printed too.
fn pretty_json_text(text: &str) -> Option<String> {
    let trimmed = text.trim_start();
    if !(trimmed.starts_with('{') || trimmed.starts_with('[')) {
        return None;
    }
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    serde_json::to_string_pretty(&value).ok()
}

impl Render for Inspector {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let columns = self.grid.read(cx).columns().to_vec();
        let column = self.column.or(self.cell.map(|c| c.1));
        let content: AnyElement = match column.and_then(|c| columns.get(c).map(|col| (c, col.info.clone()))) {
            None => v_flex()
                .gap_2()
                .p_4()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("Select a cell to see its full value and column details.")
                .into_any_element(),
            Some((ix, info)) => {
                if self.column.is_none() {
                    self.grid.update(cx, |g, cx| g.request_summaries(&[ix], cx));
                }
                v_flex()
                    .gap_4()
                    .p_3()
                    .when(self.cell.is_some() && self.column.is_none(), |this| this.child(self.render_value(&info, cx)))
                    .child(div().h_px().bg(theme.border))
                    .child(self.render_column(ix, &info, cx))
                    .into_any_element()
            }
        };
        v_flex()
            .id("inspector")
            .size_full()
            .bg(theme.background)
            .border_l_1()
            .border_color(theme.border)
            .child(content)
            .overflow_y_scrollbar()
    }
}

#[cfg(test)]
mod tests {
    use super::{pretty_json_field, pretty_json_text};

    #[test]
    fn json_pretty_printing() {
        assert_eq!(pretty_json_field(r#"{"c":{"a":1}}"#).unwrap(), "{\n  \"a\": 1\n}");
        assert_eq!(pretty_json_field(r#"{"c":5}"#), None);
        assert_eq!(pretty_json_text(r#"[1,2]"#).unwrap(), "[\n  1,\n  2\n]");
        assert_eq!(pretty_json_text("hello"), None);
        assert_eq!(pretty_json_text("{broken"), None);
    }
}
