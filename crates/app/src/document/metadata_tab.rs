//! The Metadata tab: file facts plus browsable Parquet internals.

use std::collections::HashMap;

use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tab::TabBar;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
use gpui_kit::*;
use parquetry_engine::{Dataset, MetadataTable, View, metadata_table};
use parquetry_grid::{Grid, GridState};

use crate::format;

enum TableState {
    Loading(#[allow(dead_code)] Task<()>),
    Ready(Entity<GridState>),
    Failed(SharedString),
}

pub struct MetadataTab {
    dataset: Dataset,
    selected: usize,
    tables: HashMap<MetadataTable, TableState>,
}

impl MetadataTab {
    pub fn new(dataset: Dataset, _: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            dataset,
            selected: 0,
            tables: HashMap::new(),
        };
        this.load(MetadataTable::all()[0], cx);
        this
    }

    fn load(&mut self, which: MetadataTable, cx: &mut Context<Self>) {
        if self.tables.contains_key(&which) || !self.dataset.is_parquet() {
            return;
        }
        let job = metadata_table(&self.dataset, which);
        let task = cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let result = job.await;
            let _ = this.update(cx, |this, cx| {
                let state = match result {
                    Ok(dataset) => {
                        let grid = cx.new(|cx| {
                            let mut grid = GridState::new(cx);
                            grid.set_show_summaries(false, cx);
                            grid.set_view(Some(View::identity(&dataset)), false, cx);
                            grid
                        });
                        TableState::Ready(grid)
                    }
                    Err(error) => TableState::Failed(error.to_string().into()),
                };
                this.tables.insert(which, state);
                cx.notify();
            });
        });
        self.tables.insert(which, TableState::Loading(task));
    }

    fn facts(&self) -> Vec<(&'static str, String)> {
        let ds = &self.dataset;
        let mut facts = vec![];
        if let Some(source) = ds.source() {
            facts.push(("Location", source.location.clone()));
        }
        if let Some(format) = ds.format {
            facts.push(("Format", format.label().to_string()));
        }
        facts.push(("Rows", format::count(ds.row_count)));
        facts.push(("Columns", format::count(ds.columns.len() as u64)));
        if !ds.files.is_empty() {
            facts.push(("Files", format::count(ds.files.len() as u64)));
        }
        if let Some(bytes) = ds.total_bytes {
            facts.push(("Size", format!("{} ({} bytes)", format::bytes(bytes), format::count(bytes))));
            if ds.row_count > 0 {
                facts.push(("Bytes per row", format!("{:.1}", bytes as f64 / ds.row_count as f64)));
            }
        }
        if let Some(pq) = &ds.parquet {
            facts.push(("Row groups", format::count(pq.row_groups)));
            if let Some(per_group) = ds.row_count.checked_div(pq.row_groups) {
                facts.push(("Rows per row group", format::count(per_group)));
            }
            if !pq.compression.is_empty() {
                facts.push(("Compression", pq.compression.join(", ")));
            }
            if let Some(v) = pq.format_version {
                facts.push(("Format version", v.to_string()));
            }
            if let Some(created) = &pq.created_by {
                facts.push(("Created by", created.clone()));
            }
        }
        facts.push(("Opened in", format::duration_ms(ds.open_millis)));
        facts
    }
}

impl Render for MetadataTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let facts = v_flex()
            .id("metadata-facts")
            .w(rems(20.))
            .flex_shrink_0()
            .h_full()
            .p_3()
            .gap_2()
            .border_r_1()
            .border_color(theme.border)
            .children(self.facts().into_iter().map(|(label, value)| {
                v_flex()
                    .child(div().text_xs().text_color(theme.muted_foreground).child(label))
                    .child(div().text_sm().font_family(theme.mono_font_family.clone()).whitespace_normal().child(value))
            }))
            .children(self.dataset.notes.iter().map(|note| {
                div().text_xs().text_color(theme.warning).child(note.clone())
            }))
            .overflow_y_scrollbar();

        let body: AnyElement = if !self.dataset.is_parquet() {
            div()
                .p_4()
                .text_sm()
                .text_color(theme.muted_foreground)
                .child("Row groups, column chunks and encodings are available for Parquet files.")
                .into_any_element()
        } else {
            let which = MetadataTable::all()[self.selected];
            let table: AnyElement = match self.tables.get(&which) {
                Some(TableState::Ready(grid)) => Grid::new(grid).into_any_element(),
                Some(TableState::Failed(message)) => div().p_4().text_color(theme.danger).child(message.clone()).into_any_element(),
                _ => h_flex()
                    .p_4()
                    .gap_2()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(Spinner::new().small())
                    .child("Reading footers…")
                    .into_any_element(),
            };
            v_flex()
                .size_full()
                .child(
                    h_flex().p_2().flex_shrink_0().child(
                        TabBar::new("metadata-tables")
                            .segmented()
                            .small()
                            .selected_index(self.selected)
                            .on_click(cx.listener(|this, ix: &usize, _, cx| {
                                this.selected = *ix;
                                this.load(MetadataTable::all()[*ix], cx);
                                cx.notify();
                            }))
                            .children(MetadataTable::all().iter().map(|t| t.label())),
                    ),
                )
                .child(div().flex_1().min_h_0().child(table))
                .into_any_element()
        };

        h_flex()
            .size_full()
            .items_stretch()
            .bg(theme.background)
            .child(facts)
            .child(div().flex_1().min_w_0().h_full().child(body))
    }
}
