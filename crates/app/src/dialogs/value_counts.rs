//! Value Counts: every distinct value of a column with its count, searchable, and
//! a quick way to keep or exclude the chosen values.

use std::ops::Range;
use std::time::Duration;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, IconName, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::base::TestSupportExt as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use parquetry_engine::{Filter, FilterOp, TopValue, ValueCounts, View, value_counts};

use crate::document::DatasetDocument;
use crate::format;

/// Values fetched at once; searching reaches the rest.
const LIMIT: usize = 1_000;

pub struct ValueCountsPanel {
    doc: WeakEntity<DatasetDocument>,
    view: View,
    column: usize,
    name: String,
    /// The column's kind colour, for the count bars.
    bar_color: Hsla,
    search: Entity<InputState>,
    counts: Option<ValueCounts>,
    error: Option<String>,
    loading: bool,
    /// Chosen values, in the order they were picked (`None` is null).
    chosen: Vec<Option<String>>,
    scroll: UniformListScrollHandle,
    task: Option<Task<()>>,
    _subscription: Subscription,
}

impl ValueCountsPanel {
    fn new(doc: WeakEntity<DatasetDocument>, view: View, column: usize, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let info = view.dataset.columns.get(column);
        let name = info.map(|c| c.name.clone()).unwrap_or_default();
        let bar_color = info.map(|c| parquetry_grid::chart::kind_color(c.kind, cx.theme())).unwrap_or(cx.theme().chart_1);
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("Search values…"));
        let subscription = cx.subscribe(&search, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.load(Duration::from_millis(200), cx);
            }
        });
        let mut this = Self {
            doc,
            view,
            column,
            name,
            bar_color,
            search,
            counts: None,
            error: None,
            loading: false,
            chosen: Vec::new(),
            scroll: UniformListScrollHandle::new(),
            task: None,
            _subscription: subscription,
        };
        this.load(Duration::ZERO, cx);
        this
    }

    /// Count again for the current search. Replacing the task cancels the previous
    /// query (dropping a job interrupts it).
    fn load(&mut self, delay: Duration, cx: &mut Context<Self>) {
        let query = self.search.read(cx).value().to_string();
        let (view, column) = (self.view.clone(), self.column);
        self.loading = true;
        self.task = Some(cx.spawn(async move |this, cx| {
            if !delay.is_zero() {
                cx.background_executor().timer(delay).await;
            }
            let result = value_counts(&view, column, &query, LIMIT).await;
            let _ = this.update(cx, |this, cx| {
                this.loading = false;
                match result {
                    Ok(counts) => {
                        this.counts = Some(counts);
                        this.error = None;
                    }
                    Err(error) => this.error = Some(error.to_string()),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn toggle(&mut self, value: Option<String>, cx: &mut Context<Self>) {
        if let Some(ix) = self.chosen.iter().position(|v| *v == value) {
            self.chosen.remove(ix);
        } else {
            self.chosen.push(value);
        }
        cx.notify();
    }

    /// Filters for the chosen values, or why they can't be expressed.
    fn filters(&self, keep: bool) -> Result<Vec<Filter>, &'static str> {
        filters_for(&self.name, &self.chosen, keep)
    }

    fn apply(&mut self, keep: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Ok(filters) = self.filters(keep) else { return };
        let name = self.name.clone();
        window.close_dialog(cx);
        if let Some(doc) = self.doc.upgrade() {
            doc.update(cx, |doc, cx| doc.replace_value_filters(&name, filters, window, cx));
        }
    }

    fn render_rows(&mut self, range: Range<usize>, _: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme().clone();
        let Some(counts) = &self.counts else { return Vec::new() };
        let max = counts.values.first().map(|v| v.count).unwrap_or(1).max(1) as f32;
        let total = counts.rows.max(1) as f64;
        range
            .filter_map(|ix| {
                let TopValue { value, count } = counts.values.get(ix)?.clone();
                let chosen = self.chosen.contains(&value);
                let share = count as f32 / max;
                let label: AnyElement = match &value {
                    Some(v) if v.is_empty() => div().italic().text_color(theme.muted_foreground).child("(empty)").into_any_element(),
                    Some(v) => div().min_w_0().truncate().child(v.clone()).into_any_element(),
                    None => div().italic().text_color(theme.muted_foreground).child("null").into_any_element(),
                };
                let toggle_value = value.clone();
                let keep_value = value.clone();
                Some(
                    h_flex()
                        .id(("value-count", ix))
                        .test_support()
                        .relative()
                        .h(rems(1.75))
                        .px_2()
                        .gap_2()
                        .text_sm()
                        .rounded(theme.radius)
                        .cursor_pointer()
                        .hover(|s| s.bg(theme.secondary_hover))
                        .when(chosen, |s| s.bg(theme.accent))
                        .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                            if event.click_count() >= 2 {
                                this.chosen = vec![keep_value.clone()];
                                this.apply(true, window, cx);
                            } else {
                                this.toggle(toggle_value.clone(), cx);
                            }
                        }))
                        // Share of the most frequent value, drawn behind the row.
                        .child(
                            div()
                                .absolute()
                                .left_0()
                                .top_0()
                                .bottom_0()
                                .w(relative(share))
                                .rounded(theme.radius)
                                .bg(self.bar_color.opacity(0.14)),
                        )
                        .child(
                            div().w(rems(1.)).flex_shrink_0().when(chosen, |d| d.child(Icon::new(IconName::Check).xsmall())),
                        )
                        .child(div().flex_1().min_w_0().font_family(theme.mono_font_family.clone()).child(label))
                        .child(div().flex_shrink_0().text_right().child(format::count(count)))
                        .child(
                            div()
                                .w(rems(3.5))
                                .flex_shrink_0()
                                .text_right()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(format!("{:.1}%", count as f64 / total * 100.0)),
                        )
                        .into_any_element(),
                )
            })
            .collect()
    }
}

/// `keep`: only these values; otherwise everything but them.
fn filters_for(name: &str, chosen: &[Option<String>], keep: bool) -> Result<Vec<Filter>, &'static str> {
    if chosen.is_empty() {
        return Err("Choose values first");
    }
    let values: Vec<&str> = chosen.iter().filter_map(|v| v.as_deref()).collect();
    let has_null = values.len() < chosen.len();
    // Lists are comma-separated and trimmed.
    let listable = values.iter().all(|v| !v.contains(',') && v.trim() == *v && !v.is_empty());
    if keep {
        return match (values.as_slice(), has_null) {
            ([], true) => Ok(vec![Filter::new(name, FilterOp::IsNull, "")]),
            ([one], false) => Ok(vec![Filter::new(name, FilterOp::Equals, *one)]),
            (_, true) => Err("Null can't be combined with other values"),
            (_, false) if listable => Ok(vec![Filter::new(name, FilterOp::In, values.join(", "))]),
            _ => Err("Values with commas or surrounding spaces can only be kept one at a time"),
        };
    }
    let mut filters = Vec::new();
    if has_null {
        filters.push(Filter::new(name, FilterOp::IsNotNull, ""));
    }
    match values.as_slice() {
        [] => {}
        [one] => filters.push(Filter::new(name, FilterOp::NotEquals, *one)),
        _ if listable => filters.push(Filter::new(name, FilterOp::NotIn, values.join(", "))),
        _ => filters.extend(values.iter().map(|v| Filter::new(name, FilterOp::NotEquals, *v))),
    }
    Ok(filters)
}

impl ValueCountsPanel {
    fn render_footer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let chosen = self.chosen.len();
        let hint = if chosen == 0 {
            Ok("Click values to choose them; double-click keeps one".to_string())
        } else {
            self.filters(true).map(|_| format!("{chosen} chosen"))
        };
        let keep_ok = chosen > 0 && hint.is_ok();
        let exclude_ok = self.filters(false).is_ok();
        h_flex()
            .pt_2()
            .gap_2()
            .child(
                div()
                    .flex_1()
                    .text_xs()
                    .text_color(if hint.is_ok() { theme.muted_foreground } else { theme.danger })
                    .child(match hint {
                        Ok(text) => text,
                        Err(reason) => reason.to_string(),
                    }),
            )
            .child(
                Button::new("exclude-values")
                    .label("Exclude")
                    .outline()
                    .disabled(!exclude_ok)
                    .on_click(cx.listener(|this, _, window, cx| this.apply(false, window, cx))),
            )
            .child(
                Button::new("keep-values")
                    .label("Keep Only")
                    .primary()
                    .disabled(!keep_ok)
                    .on_click(cx.listener(|this, _, window, cx| this.apply(true, window, cx))),
            )
    }
}

impl Render for ValueCountsPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let count = self.counts.as_ref().map(|c| c.values.len()).unwrap_or(0);
        let searching = !self.search.read(cx).value().trim().is_empty();
        let summary = match &self.counts {
            Some(c) if c.distinct > c.values.len() as u64 => format!(
                "{} most frequent of {} {}values · search to find others",
                format::count(c.values.len() as u64),
                format::count(c.distinct),
                if searching { "matching " } else { "" }
            ),
            Some(c) => format!(
                "{} · {} of {} rows",
                format::plural(c.distinct, "value", "values"),
                format::count(c.matching_rows),
                format::count(c.rows)
            ),
            None => String::new(),
        };
        v_flex()
            .gap_2()
            .child(
                h_flex()
                    .gap_2()
                    .child(div().flex_1().child(Input::new(&self.search).small()))
                    .when(self.loading, |this| this.child(Spinner::new().small())),
            )
            .child(div().text_xs().text_color(theme.muted_foreground).child(summary))
            .child(match &self.error {
                Some(error) => div().h(px(360.)).p_2().text_sm().text_color(theme.danger).child(error.clone()).into_any_element(),
                None if count == 0 && !self.loading => div()
                    .h(px(360.))
                    .p_2()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child("No values")
                    .into_any_element(),
                None => uniform_list("value-counts", count, cx.processor(Self::render_rows))
                    .track_scroll(&self.scroll)
                    .h(px(360.))
                    .into_any_element(),
            })
            .child(self.render_footer(cx))
    }
}

pub fn open(doc: Entity<DatasetDocument>, view: View, column: usize, window: &mut Window, cx: &mut App) {
    let weak = doc.downgrade();
    let panel = cx.new(|cx| ValueCountsPanel::new(weak, view, column, window, cx));
    let name = panel.read(cx).name.clone();
    let search = panel.read(cx).search.clone();
    crate::dialogs::focus_after_open(search.clone(), window);
    window.open_dialog(cx, move |dialog, _, _| dialog.title(format!("Values of {name}")).width(px(560.)).child(panel.clone()));
    search.update(cx, |state, cx| state.focus(window, cx));
}

#[cfg(test)]
mod tests {
    use super::filters_for;
    use parquetry_engine::{Filter, FilterOp};

    fn v(s: &str) -> Option<String> {
        Some(s.to_string())
    }

    #[test]
    fn chosen_values_become_filters() {
        assert_eq!(filters_for("c", &[v("a")], true).unwrap(), vec![Filter::new("c", FilterOp::Equals, "a")]);
        assert_eq!(filters_for("c", &[None], true).unwrap(), vec![Filter::new("c", FilterOp::IsNull, "")]);
        assert_eq!(filters_for("c", &[v("a"), v("b")], true).unwrap(), vec![Filter::new("c", FilterOp::In, "a, b")]);
        assert!(filters_for("c", &[v("a"), None], true).is_err());
        assert!(filters_for("c", &[v("a,b"), v("c")], true).is_err());
        assert_eq!(filters_for("c", &[v("a,b")], true).unwrap(), vec![Filter::new("c", FilterOp::Equals, "a,b")]);
        assert!(filters_for("c", &[], true).is_err());

        assert_eq!(filters_for("c", &[v("a")], false).unwrap(), vec![Filter::new("c", FilterOp::NotEquals, "a")]);
        assert_eq!(
            filters_for("c", &[v("a"), v("b"), None], false).unwrap(),
            vec![Filter::new("c", FilterOp::IsNotNull, ""), Filter::new("c", FilterOp::NotIn, "a, b")]
        );
        assert_eq!(
            filters_for("c", &[v("a,b"), v("c")], false).unwrap(),
            vec![Filter::new("c", FilterOp::NotEquals, "a,b"), Filter::new("c", FilterOp::NotEquals, "c")]
        );
    }
}
