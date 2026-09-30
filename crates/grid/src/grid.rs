//! `Grid`: the focusable, keyboard-driven wrapper around [`GridElement`].

use std::rc::Rc;

use gpui_kit::component::menu::{ContextMenuExt as _, PopupMenu};
use gpui_kit::component::{ActiveTheme as _, h_flex, v_flex};
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use parquetry_engine::{ChartKind, ColumnSummary, CopyFormat};

use crate::chart::{axis_label, compact_count, thousands, top_value_bars};
use crate::element::GridElement;
use crate::selection::Movement;
use crate::state::{GridState, Hit, SummaryState};

const CONTEXT: &str = "DataGrid";

gpui_kit::actions!(
    data_grid,
    [
        MoveUp,
        MoveDown,
        MoveLeft,
        MoveRight,
        SelectUp,
        SelectDown,
        SelectLeft,
        SelectRight,
        PageUp,
        PageDown,
        SelectPageUp,
        SelectPageDown,
        MoveToRowStart,
        MoveToRowEnd,
        MoveToFirstRow,
        MoveToLastRow,
        SelectToFirstRow,
        SelectToLastRow,
        SelectAll,
        Copy,
        CopyWithHeaders,
        Inspect,
        ClearSelection,
    ]
);

/// Register the grid's key bindings. Call once at startup.
pub fn init(cx: &mut App) {
    let ctx = Some(CONTEXT);
    cx.bind_keys([
        KeyBinding::new("up", MoveUp, ctx),
        KeyBinding::new("down", MoveDown, ctx),
        KeyBinding::new("left", MoveLeft, ctx),
        KeyBinding::new("right", MoveRight, ctx),
        KeyBinding::new("tab", MoveRight, ctx),
        KeyBinding::new("shift-tab", MoveLeft, ctx),
        KeyBinding::new("shift-up", SelectUp, ctx),
        KeyBinding::new("shift-down", SelectDown, ctx),
        KeyBinding::new("shift-left", SelectLeft, ctx),
        KeyBinding::new("shift-right", SelectRight, ctx),
        KeyBinding::new("pageup", PageUp, ctx),
        KeyBinding::new("pagedown", PageDown, ctx),
        KeyBinding::new("shift-pageup", SelectPageUp, ctx),
        KeyBinding::new("shift-pagedown", SelectPageDown, ctx),
        KeyBinding::new("home", MoveToRowStart, ctx),
        KeyBinding::new("end", MoveToRowEnd, ctx),
        // `secondary` is ⌘ on macOS and Ctrl elsewhere.
        KeyBinding::new("secondary-left", MoveToRowStart, ctx),
        KeyBinding::new("secondary-right", MoveToRowEnd, ctx),
        KeyBinding::new("secondary-up", MoveToFirstRow, ctx),
        KeyBinding::new("secondary-down", MoveToLastRow, ctx),
        KeyBinding::new("secondary-shift-up", SelectToFirstRow, ctx),
        KeyBinding::new("secondary-shift-down", SelectToLastRow, ctx),
        // Windows and Linux convention (Excel, file managers).
        KeyBinding::new("ctrl-home", MoveToFirstRow, ctx),
        KeyBinding::new("ctrl-end", MoveToLastRow, ctx),
        KeyBinding::new("ctrl-shift-home", SelectToFirstRow, ctx),
        KeyBinding::new("ctrl-shift-end", SelectToLastRow, ctx),
        KeyBinding::new("secondary-a", SelectAll, ctx),
        KeyBinding::new("secondary-c", Copy, ctx),
        KeyBinding::new("secondary-shift-c", CopyWithHeaders, ctx),
        KeyBinding::new("enter", Inspect, ctx),
        KeyBinding::new("space", Inspect, ctx),
        KeyBinding::new("escape", ClearSelection, ctx),
    ]);
}

type MenuBuilder = Rc<dyn Fn(PopupMenu, Option<Hit>, &mut Window, &mut Context<PopupMenu>) -> PopupMenu>;

/// A data grid bound to a [`GridState`].
#[derive(IntoElement)]
pub struct Grid {
    state: Entity<GridState>,
    context_menu: Option<MenuBuilder>,
}

impl Grid {
    pub fn new(state: &Entity<GridState>) -> Self {
        Self {
            state: state.clone(),
            context_menu: None,
        }
    }

    /// Build the right-click menu. Receives what was right-clicked.
    pub fn context_menu(
        mut self,
        builder: impl Fn(PopupMenu, Option<Hit>, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static,
    ) -> Self {
        self.context_menu = Some(Rc::new(builder));
        self
    }
}

impl RenderOnce for Grid {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let state = self.state.clone();
        let focus = state.read(cx).focus_handle(cx);
        let tooltip = header_tooltip(&state, window, cx);
        let menu_state = state.clone();
        let builder = self.context_menu.clone();

        macro_rules! nav {
            ($action:ty, $movement:expr, $extend:expr) => {{
                let state = state.clone();
                move |_: &$action, _: &mut Window, cx: &mut App| {
                    state.update(cx, |s, cx| s.move_selection($movement, $extend, cx));
                }
            }};
        }

        div()
            .id("data-grid-root")
            .test_support()
            .size_full()
            .relative()
            .key_context(CONTEXT)
            .track_focus(&focus)
            .on_action(nav!(MoveUp, Movement::Up, false))
            .on_action(nav!(MoveDown, Movement::Down, false))
            .on_action(nav!(MoveLeft, Movement::Left, false))
            .on_action(nav!(MoveRight, Movement::Right, false))
            .on_action(nav!(SelectUp, Movement::Up, true))
            .on_action(nav!(SelectDown, Movement::Down, true))
            .on_action(nav!(SelectLeft, Movement::Left, true))
            .on_action(nav!(SelectRight, Movement::Right, true))
            .on_action(nav!(PageUp, Movement::PageUp, false))
            .on_action(nav!(PageDown, Movement::PageDown, false))
            .on_action(nav!(SelectPageUp, Movement::PageUp, true))
            .on_action(nav!(SelectPageDown, Movement::PageDown, true))
            .on_action(nav!(MoveToRowStart, Movement::RowStart, false))
            .on_action(nav!(MoveToRowEnd, Movement::RowEnd, false))
            .on_action(nav!(MoveToFirstRow, Movement::FirstRow, false))
            .on_action(nav!(MoveToLastRow, Movement::LastRow, false))
            .on_action(nav!(SelectToFirstRow, Movement::FirstRow, true))
            .on_action(nav!(SelectToLastRow, Movement::LastRow, true))
            .on_action(window.listener_for(&state, |s: &mut GridState, _: &SelectAll, _, cx| s.select_all(cx)))
            .on_action(window.listener_for(&state, |s: &mut GridState, _: &ClearSelection, _, cx| s.clear_selection(cx)))
            .on_action(window.listener_for(&state, |s: &mut GridState, _: &Copy, _, cx| {
                s.copy_selection(CopyFormat::Tsv, false, cx)
            }))
            .on_action(window.listener_for(&state, |s: &mut GridState, _: &CopyWithHeaders, _, cx| {
                s.copy_selection(CopyFormat::Tsv, true, cx)
            }))
            .on_action(window.listener_for(&state, |s: &mut GridState, _: &Inspect, _, cx| {
                if let Some((row, column)) = s.active_cell() {
                    cx.emit(crate::GridEvent::InspectCell { row, column });
                }
            }))
            .child(GridElement::new(state.clone()))
            .children(tooltip)
            .context_menu(move |menu, window, cx| {
                let position = window.mouse_position();
                let hit = menu_state.update(cx, |s, cx| s.prepare_context_menu(position, cx));
                match &builder {
                    Some(builder) => builder(menu, hit, window, cx),
                    None => menu,
                }
            })
    }
}

/// Details for the hovered header chart, shown near the pointer.
fn header_tooltip(state: &Entity<GridState>, _window: &mut Window, cx: &mut App) -> Option<AnyElement> {
    let s = state.read(cx);
    if s.drag.is_some() {
        return None;
    }
    let (Some(Hit::HeaderChart(col, item)), Some(position)) = (s.hover, s.hover_position) else {
        return None;
    };
    let column = *s.display_columns().get(col)?;
    let info = s.columns().get(column)?.info.clone();
    let summary = match s.summary(column)? {
        SummaryState::Ready(summary) => summary.clone(),
        SummaryState::Loading => return None,
        SummaryState::Failed(message) => {
            let message = message.clone();
            return Some(tooltip_card(cx, vec![("Summary failed".into(), message.to_string())], info.name.clone(), None));
        }
    };
    let rows = summary_rows(&summary);
    let highlight = item.and_then(|ix| {
        let text = hovered_item_text(&summary, ix)?;
        Some(if summary.filters_for_bar(ix).is_some() { format!("{text} · click to filter") } else { text })
    });
    let card = tooltip_card(cx, rows, format!("{} · {}", info.name, info.type_label()), highlight);
    Some(
        deferred(
            anchored()
                .position(position)
                .offset(point(px(12.), px(16.)))
                .snap_to_window_with_margin(px(8.))
                .child(card),
        )
        .with_priority(2)
        .into_any_element(),
    )
}

fn tooltip_card(cx: &App, rows: Vec<(String, String)>, title: String, highlight: Option<String>) -> AnyElement {
    let theme = cx.theme();
    v_flex()
        .id("grid-summary-tooltip")
        .max_w(rems(24.))
        .gap_1()
        .px_3()
        .py_2()
        .bg(theme.popover)
        .text_color(theme.popover_foreground)
        .border_1()
        .border_color(theme.border)
        .rounded(theme.radius)
        .shadow_lg()
        .text_xs()
        .child(div().font_weight(FontWeight::SEMIBOLD).text_sm().child(title))
        .when_some(highlight, |this, text| {
            this.child(
                div()
                    .px_2()
                    .py_1()
                    .rounded(theme.radius)
                    .bg(theme.accent)
                    .text_color(theme.accent_foreground)
                    .child(text),
            )
        })
        .children(rows.into_iter().map(|(label, value)| {
            h_flex()
                .gap_3()
                .justify_between()
                .child(div().text_color(theme.muted_foreground).child(label))
                .child(div().font_family(theme.mono_font_family.clone()).child(value))
        }))
        .into_any_element()
}

fn summary_rows(summary: &ColumnSummary) -> Vec<(String, String)> {
    let mut rows = Vec::new();
    rows.push(("Rows".into(), thousands(summary.rows)));
    if summary.sampled {
        rows.push(("Sampled".into(), format!("{} rows", thousands(summary.scanned_rows))));
    }
    let nulls = match summary.exact_nulls {
        Some(n) => format!("{} ({:.1}%)", thousands(n), summary.null_fraction() * 100.0),
        None => format!("≈ {:.1}%", summary.null_fraction() * 100.0),
    };
    rows.push(("Nulls".into(), nulls));
    if let Some(distinct) = summary.distinct {
        let prefix = if summary.distinct_exact && !summary.sampled { "" } else { "≈ " };
        rows.push(("Distinct".into(), format!("{prefix}{}", thousands(distinct))));
    }
    let exact = if summary.min_max_exact || !summary.sampled { "" } else { " (sample)" };
    if let Some(min) = &summary.min {
        rows.push((format!("Min{exact}"), shorten(min)));
    }
    if let Some(max) = &summary.max {
        rows.push((format!("Max{exact}"), shorten(max)));
    }
    if let Some(mean) = summary.mean {
        rows.push(("Mean".into(), parquetry_engine::format_number(mean)));
    }
    if let Some(std) = summary.std_dev {
        rows.push(("Std dev".into(), parquetry_engine::format_number(std)));
    }
    if let Some([p25, p50, p75]) = &summary.quantiles {
        rows.push(("25% / 50% / 75%".into(), format!("{} / {} / {}", shorten(p25), shorten(p50), shorten(p75))));
    }
    if let Some((min, avg, max)) = summary.text_length {
        rows.push(("Length".into(), format!("{min}–{max} (avg {avg:.1})")));
    }
    if summary.preferred_chart() == ChartKind::TopValues {
        for bar in top_value_bars(summary).into_iter().take(5) {
            rows.push((shorten(&bar.label), percent(bar.count, summary.scanned_rows)));
        }
    }
    rows
}

fn hovered_item_text(summary: &ColumnSummary, ix: usize) -> Option<String> {
    match summary.preferred_chart() {
        ChartKind::Histogram => {
            let bin = summary.histogram.get(ix)?;
            Some(format!(
                "{} – {}: {} ({})",
                axis_label(summary, bin.start),
                axis_label(summary, bin.end),
                compact_count(bin.count),
                percent(bin.count, summary.scanned_rows)
            ))
        }
        ChartKind::TopValues => {
            let bar = top_value_bars(summary).into_iter().nth(ix)?;
            Some(format!(
                "{}: {} ({})",
                shorten(&bar.label),
                thousands(bar.count),
                percent(bar.count, summary.scanned_rows)
            ))
        }
        ChartKind::None => None,
    }
}

fn percent(count: u64, total: u64) -> String {
    if total == 0 {
        return "0%".into();
    }
    let p = count as f64 * 100.0 / total as f64;
    if p < 0.1 && count > 0 {
        "<0.1%".into()
    } else {
        format!("{p:.1}%")
    }
}

fn shorten(text: &str) -> String {
    crate::layout::truncate_chars(text, 48).unwrap_or_else(|| text.to_string())
}
