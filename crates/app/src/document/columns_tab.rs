//! The Columns tab: every column at a glance, with its distribution and key numbers.

use std::ops::Range;

use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use parquetry_engine::{ColumnSummary, format_number};
use parquetry_grid::chart::{ChartColors, chart_placeholder, compact_count, null_label, paint_chart};
use parquetry_grid::{GridState, SummaryState};

/// Emitted when a column row is chosen.
pub struct RevealColumn(pub usize);

pub struct ColumnsTab {
    grid: Entity<GridState>,
    scroll: UniformListScrollHandle,
    _observe: Subscription,
}

impl EventEmitter<RevealColumn> for ColumnsTab {}

impl ColumnsTab {
    pub fn new(grid: Entity<GridState>, cx: &mut Context<Self>) -> Self {
        let observe = cx.observe(&grid, |_, _, cx| cx.notify());
        Self {
            grid,
            scroll: UniformListScrollHandle::new(),
            _observe: observe,
        }
    }

    fn render_rows(&mut self, range: Range<usize>, _: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme().clone();
        let columns: Vec<usize> = range.clone().collect();
        // Load summaries for the rows on screen.
        self.grid.update(cx, |g, cx| g.request_summaries(&columns, cx));
        let grid = self.grid.read(cx);
        range
            .filter_map(|ix| {
                let column = grid.columns().get(ix)?;
                let info = column.info.clone();
                let summary = match grid.summary(ix) {
                    Some(SummaryState::Ready(s)) => Some(s.clone()),
                    _ => None,
                };
                let hidden = grid.is_hidden(ix);
                let colors = ChartColors {
                    bar: theme.chart_1.opacity(0.85),
                    bar_hover: theme.chart_1,
                    other: theme.muted_foreground.opacity(0.35),
                    null: theme.warning.opacity(0.6),
                    baseline: theme.border,
                };
                let chart_summary = summary.clone();
                let chart = canvas(
                    |_, _, _| (),
                    move |bounds, _, window, _| {
                        if let Some(summary) = &chart_summary {
                            paint_chart(summary, bounds, None, colors, window);
                        }
                    },
                )
                .w(rems(10.))
                .h(rems(1.75));
                let (nulls, distinct, min, max, mean) = describe(summary.as_deref());
                let placeholder = summary
                    .as_deref()
                    .filter(|s| s.preferred_chart() == parquetry_engine::ChartKind::None)
                    .map(chart_placeholder);
                Some(
                    h_flex()
                        .id(("column-row", ix))
                        .h(rems(2.75))
                        .px_3()
                        .gap_3()
                        .border_b_1()
                        .border_color(theme.table_row_border)
                        .when(ix % 2 == 1, |this| this.bg(theme.table_even))
                        .hover(|this| this.bg(theme.table_hover))
                        .cursor_pointer()
                        .on_click(cx.listener(move |_, _, _, cx| cx.emit(RevealColumn(ix))))
                        .child(
                            div()
                                .w(rems(2.5))
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(format!("{ix}")),
                        )
                        .child(
                            v_flex()
                                .w(rems(16.))
                                .overflow_hidden()
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_ellipsis()
                                        .when(hidden, |this| this.text_color(theme.muted_foreground))
                                        .child(info.name.clone()),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .font_family(theme.mono_font_family.clone())
                                        .text_color(theme.muted_foreground)
                                        .text_ellipsis()
                                        .child(match &info.physical_type {
                                            Some(physical) => format!("{} · {physical}", info.type_label()),
                                            None => info.type_label(),
                                        }),
                                ),
                        )
                        .child(match placeholder {
                            Some(text) => div()
                                .w(rems(10.))
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(text)
                                .into_any_element(),
                            None => chart.into_any_element(),
                        })
                        .child(cell(&theme, 7.0, nulls, true))
                        .child(cell(&theme, 7.0, distinct, true))
                        .child(cell(&theme, 11.0, min, false))
                        .child(cell(&theme, 11.0, max, false))
                        .child(cell(&theme, 8.0, mean, true))
                        .into_any_element(),
                )
            })
            .collect()
    }
}

fn cell(theme: &gpui_kit::component::Theme, width: f32, text: String, right: bool) -> Div {
    div()
        .w(rems(width))
        .flex_shrink_0()
        .text_xs()
        .font_family(theme.mono_font_family.clone())
        .text_ellipsis()
        .overflow_hidden()
        .when(right, |this| this.text_right())
        .child(text)
}

fn describe(summary: Option<&ColumnSummary>) -> (String, String, String, String, String) {
    let Some(s) = summary else {
        return ("…".into(), "…".into(), String::new(), String::new(), String::new());
    };
    let nulls = {
        let label = null_label(s);
        if label.is_empty() { "0".into() } else { label.trim_end_matches(" null").to_string() }
    };
    let distinct = s.distinct.map(compact_count).unwrap_or_default();
    let min = s.min.clone().unwrap_or_default();
    let max = s.max.clone().unwrap_or_default();
    let mean = s.mean.map(format_number).unwrap_or_default();
    (nulls, distinct, min, max, mean)
}

impl Render for ColumnsTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let count = self.grid.read(cx).columns().len();
        let header = |label: &'static str, width: f32, right: bool| {
            div()
                .w(rems(width))
                .flex_shrink_0()
                .text_xs()
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(theme.muted_foreground)
                .when(right, |this| this.text_right())
                .child(label)
        };
        v_flex()
            .size_full()
            .bg(theme.table)
            .child(
                h_flex()
                    .h(rems(2.25))
                    .px_3()
                    .gap_3()
                    .flex_shrink_0()
                    .bg(theme.table_head)
                    .border_b_1()
                    .border_color(theme.border)
                    .child(header("#", 2.5, false))
                    .child(header("Column", 16.0, false))
                    .child(header("Distribution", 10.0, false))
                    .child(header("Nulls", 7.0, true))
                    .child(header("Distinct", 7.0, true))
                    .child(header("Min", 11.0, false))
                    .child(header("Max", 11.0, false))
                    .child(header("Mean", 8.0, true)),
            )
            .child(
                uniform_list("columns-overview", count, cx.processor(Self::render_rows))
                    .track_scroll(&self.scroll)
                    .flex_1()
                    .min_h_0(),
            )
    }
}
