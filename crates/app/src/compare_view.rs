//! Comparison results: schema differences, value changes, and the differing rows.

use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tab::TabBar;
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use parquetry_engine::{CompareOptions, Comparison, Dataset, Job, View, compare};
use parquetry_grid::{Grid, GridState};

use crate::format;

enum State {
    Running(#[allow(dead_code)] Task<()>),
    Done { result: Box<Comparison>, grids: Vec<(String, Entity<GridState>)> },
    Failed(SharedString),
}

pub struct CompareView {
    title: SharedString,
    state: State,
    tab: usize,
}

impl CompareView {
    pub fn new(left: Dataset, right: Dataset, options: CompareOptions, cx: &mut Context<Self>) -> Self {
        let title = format!("{} ↔ {}", left.name, right.name).into();
        let job: Job<Comparison> = compare(&left, &right, options);
        let task = cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let result = job.await;
            let _ = this.update(cx, |this, cx| {
                this.state = match result {
                    Ok(result) => {
                        let mut grids = Vec::new();
                        let mut add = |label: String, dataset: &Dataset, cx: &mut Context<Self>| {
                            let view = View::identity(dataset);
                            let grid = cx.new(|cx| {
                                let mut g = GridState::new(cx);
                                g.set_view(Some(view), false, cx);
                                g
                            });
                            grids.push((label, grid));
                        };
                        if let Some(changed) = &result.changed {
                            add(format!("Changed ({})", format::count(changed.row_count)), changed, cx);
                        }
                        add(format!("Only in {} ({})", result.left_name, format::count(result.only_left.row_count)), &result.only_left, cx);
                        add(format!("Only in {} ({})", result.right_name, format::count(result.only_right.row_count)), &result.only_right, cx);
                        State::Done { result: Box::new(result), grids }
                    }
                    Err(error) => State::Failed(error.to_string().into()),
                };
                cx.notify();
            });
        });
        Self {
            title,
            state: State::Running(task),
            tab: 0,
        }
    }

    pub fn title(&self) -> SharedString {
        self.title.clone()
    }

    /// The comparison, once finished.
    #[cfg(test)]
    pub fn result(&self) -> Option<&Comparison> {
        match &self.state {
            State::Done { result, .. } => Some(result),
            _ => None,
        }
    }

    #[cfg(test)]
    pub fn error(&self) -> Option<SharedString> {
        match &self.state {
            State::Failed(message) => Some(message.clone()),
            _ => None,
        }
    }
}

impl Render for CompareView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        match &self.state {
            State::Running(_) => h_flex()
                .size_full()
                .justify_center()
                .gap_2()
                .child(Spinner::new())
                .child(div().text_sm().child(format!("Comparing {}…", self.title)))
                .into_any_element(),
            State::Failed(message) => v_flex()
                .size_full()
                .p_6()
                .gap_2()
                .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).child("Comparison failed"))
                .child(div().text_sm().text_color(theme.danger).child(message.clone()))
                .into_any_element(),
            State::Done { result, grids } => {
                let r = result;
                let mut schema: Vec<AnyElement> = Vec::new();
                for c in &r.only_left_columns {
                    schema.push(div().text_sm().child(format!("− {} ({}) only in {}", c.name, c.type_label(), r.left_name)).into_any_element());
                }
                for c in &r.only_right_columns {
                    schema.push(div().text_sm().child(format!("+ {} ({}) only in {}", c.name, c.type_label(), r.right_name)).into_any_element());
                }
                for t in &r.type_changes {
                    schema.push(div().text_sm().child(format!("~ {}: {} → {}", t.column, t.left, t.right)).into_any_element());
                }
                let summary = v_flex()
                    .id("compare-summary")
                    .w(rems(22.))
                    .flex_shrink_0()
                    .h_full()
                    .p_4()
                    .gap_3()
                    .border_r_1()
                    .border_color(theme.border)
                    .child(
                        h_flex()
                            .gap_2()
                            .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).child("Summary"))
                            .child(if r.is_identical() {
                                Tag::success().small().child("Identical")
                            } else {
                                Tag::warning().small().child("Different")
                            }),
                    )
                    .child(div().text_sm().child(format!("{}: {}", r.left_name, format::plural(r.left_rows, "row", "rows"))))
                    .child(div().text_sm().child(format!("{}: {}", r.right_name, format::plural(r.right_rows, "row", "rows"))))
                    .child(div().text_sm().text_color(theme.muted_foreground).child(if r.keys.is_empty() {
                        "Compared whole rows (no key)".to_string()
                    } else {
                        format!("Matched on {}", r.keys.join(", "))
                    }))
                    .when(r.duplicate_keys > 0, |this| {
                        this.child(div().text_sm().text_color(theme.warning).child(format!(
                            "{} keys appear more than once; matches may repeat",
                            format::count(r.duplicate_keys)
                        )))
                    })
                    .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child("Schema"))
                    .map(|this| {
                        if schema.is_empty() {
                            this.child(div().text_sm().text_color(theme.muted_foreground).child(format!("Same {} columns", r.common_columns.len())))
                        } else {
                            this.children(schema)
                        }
                    })
                    .when(!r.column_changes.is_empty(), |this| {
                        this.child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child("Changed values"))
                            .children(r.column_changes.iter().map(|c| {
                                h_flex()
                                    .justify_between()
                                    .text_sm()
                                    .child(c.column.clone())
                                    .child(div().text_color(theme.muted_foreground).child(format::plural(c.changed_rows, "row", "rows")))
                            }))
                    })
                    .child(div().text_xs().text_color(theme.muted_foreground).child(format!("Compared in {}", format::duration_ms(r.millis))))
                    .overflow_y_scrollbar();
                let tab = self.tab.min(grids.len().saturating_sub(1));
                let grid = grids.get(tab).map(|(_, g)| g.clone());
                h_flex()
                    .size_full()
                    .items_stretch()
                    .child(summary)
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                h_flex().p_2().flex_shrink_0().child(
                                    TabBar::new("compare-tabs")
                                        .segmented()
                                        .small()
                                        .selected_index(tab)
                                        .on_click(cx.listener(|this, ix: &usize, _, cx| {
                                            this.tab = *ix;
                                            cx.notify();
                                        }))
                                        .children(grids.iter().map(|(label, _)| SharedString::from(label.clone()))),
                                ),
                            )
                            .child(div().flex_1().min_h_0().children(grid.map(|g| Grid::new(&g)))),
                    )
                    .into_any_element()
            }
        }
    }
}
