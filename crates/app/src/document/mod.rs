//! A dataset document: one opened file/folder/URL, shown as a grid with search,
//! filters, sort, column overview, metadata, SQL and an inspector.

mod columns_tab;
mod inspector;
mod metadata_tab;

use std::time::{Duration, Instant};

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{PopupMenu, PopupMenuItem};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::tab::TabBar;
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, WindowExt as _, h_flex, h_resizable, resizable_panel, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::component::menu::DropdownMenu as _;
use gpui_kit::component::Selectable as _;
use gpui_kit::*;
use parquetry_engine::{
    Canceller, ColumnKind, CopyFormat, Dataset, Filter, FilterOp, SortKey, StatsMode, View, ViewSpec,
};
use parquetry_grid::{Grid, GridEvent, GridState, Hit};

use crate::actions::*;
use crate::app_state::AppState;
use crate::format;
use crate::sql_panel::{SqlEvent, SqlPanel};

pub use columns_tab::{ColumnsTab, RevealColumn};
pub use inspector::Inspector;
pub use metadata_tab::MetadataTab;

const REVEAL_IN_FILE_MANAGER: &str = if cfg!(target_os = "macos") {
    "Reveal in Finder"
} else if cfg!(windows) {
    "Show in File Explorer"
} else {
    "Show in File Manager"
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocTab {
    Data,
    Columns,
    Metadata,
    Sql,
}

impl DocTab {
    const ALL: [DocTab; 4] = [DocTab::Data, DocTab::Columns, DocTab::Metadata, DocTab::Sql];

    fn label(self) -> &'static str {
        match self {
            DocTab::Data => "Data",
            DocTab::Columns => "Columns",
            DocTab::Metadata => "Metadata",
            DocTab::Sql => "SQL",
        }
    }
}

/// Requests for the workspace.
pub enum DocumentEvent {
    /// Open a dataset (e.g. a SQL result) in a new tab.
    OpenDataset { dataset: Dataset, title: String },
    /// Open the compare dialog with this document preselected.
    Compare,
}

/// Removes one filter chip's condition.
type RemoveChip = Box<dyn Fn(&mut DatasetDocument, &mut Window, &mut Context<DatasetDocument>)>;

struct Build {
    _task: Task<()>,
    canceller: Canceller,
    started: Instant,
    label: SharedString,
}

struct HeaderMenu {
    menu: Entity<PopupMenu>,
    anchor: Bounds<Pixels>,
    _dismiss: Subscription,
}

pub struct DatasetDocument {
    pub dataset: Dataset,
    title: SharedString,
    spec: ViewSpec,
    pub grid: Entity<GridState>,
    tab: DocTab,
    search: Entity<InputState>,
    columns_tab: Option<Entity<ColumnsTab>>,
    metadata_tab: Option<Entity<MetadataTab>>,
    sql: Option<Entity<SqlPanel>>,
    inspector: Entity<Inspector>,
    inspector_open: bool,
    build: Option<Build>,
    materialize: Option<Task<()>>,
    error: Option<SharedString>,
    header_menu: Option<HeaderMenu>,
    focus_handle: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<DocumentEvent> for DatasetDocument {}

impl Focusable for DatasetDocument {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl DatasetDocument {
    pub fn new(dataset: Dataset, title: Option<String>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let show_summaries = AppState::settings(cx).show_summaries;
        let view = View::identity(&dataset);
        let grid = cx.new(|cx| {
            let mut grid = GridState::new(cx);
            grid.set_show_summaries(show_summaries, cx);
            grid.set_view(Some(view), false, cx);
            grid
        });
        let search = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("Search all columns")
                .clean_on_escape()
        });
        let inspector = cx.new(|cx| Inspector::new(grid.clone(), cx));
        let mut subscriptions = vec![
            cx.subscribe_in(&grid, window, Self::on_grid_event),
            cx.subscribe_in(&search, window, Self::on_search_event),
        ];
        subscriptions.push(cx.observe(&grid, |_, _, cx| cx.notify()));
        let title = title.unwrap_or_else(|| dataset.name.clone()).into();
        Self {
            dataset,
            title,
            spec: ViewSpec::default(),
            grid,
            tab: DocTab::Data,
            search,
            columns_tab: None,
            metadata_tab: None,
            sql: None,
            inspector,
            inspector_open: false,
            build: None,
            materialize: None,
            error: None,
            header_menu: None,
            focus_handle: cx.focus_handle(),
            _subscriptions: subscriptions,
        }
    }

    pub fn title(&self) -> SharedString {
        self.title.clone()
    }

    pub fn is_busy(&self) -> bool {
        self.build.is_some()
    }

    /// Move keyboard focus to what the document shows: the grid, the SQL editor,
    /// or the document itself.
    pub fn focus_grid(this: &Entity<Self>, window: &mut Window, cx: &mut App) {
        let doc = this.read(cx);
        match doc.tab {
            DocTab::Data => {
                let handle = doc.grid.read(cx).focus_handle(cx);
                handle.focus(window, cx);
            }
            DocTab::Sql if doc.sql.is_some() => {
                let sql = doc.sql.clone().unwrap();
                SqlPanel::focus_editor(&sql, window, cx);
            }
            _ => {
                let handle = doc.focus_handle.clone();
                handle.focus(window, cx);
            }
        }
    }

    fn focus_own_grid(&self, window: &mut Window, cx: &mut App) {
        let handle = self.grid.read(cx).focus_handle(cx);
        handle.focus(window, cx);
    }

    // ------------------------------------------------------------ view building

    /// Apply a new filter/search/sort. The previous view stays on screen until
    /// the new one is ready; a newer request cancels an older one.
    pub fn apply_spec(&mut self, spec: ViewSpec, window: &mut Window, cx: &mut Context<Self>) {
        if spec == self.spec && self.build.is_none() && self.error.is_none() {
            return;
        }
        self.spec = spec.clone();
        self.error = None;
        self.materialize = None;
        if let Some(build) = self.build.take() {
            build.canceller.cancel();
        }
        if spec.is_identity() {
            let view = View::identity(&self.dataset);
            self.grid.update(cx, |g, cx| g.set_view(Some(view), false, cx));
            cx.notify();
            return;
        }
        let label: SharedString = if spec.sort.is_empty() {
            "Filtering".into()
        } else if spec.has_filter() {
            "Filtering and sorting".into()
        } else {
            "Sorting".into()
        };
        let job = View::build(&self.dataset, spec);
        let canceller = job.canceller();
        let task = cx.spawn_in(window, async move |this, cx| {
            let result = job.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.build = None;
                match result {
                    Ok(view) => {
                        let materialize = view.materialize_if_small();
                        this.grid.update(cx, |g, cx| g.set_view(Some(view), false, cx));
                        if let Some(job) = materialize {
                            this.materialize = Some(cx.spawn(async move |_, _| {
                                let _ = job.await;
                            }));
                        }
                    }
                    Err(error) if error.is_cancelled() => {}
                    Err(error) => {
                        this.error = Some(error.to_string().into());
                        window.push_notification(
                            gpui_kit::component::notification::Notification::error(error.to_string())
                                .title("Couldn’t apply filter"),
                            cx,
                        );
                    }
                }
                cx.notify();
            });
        });
        self.build = Some(Build {
            _task: task,
            canceller,
            started: Instant::now(),
            label,
        });
        cx.notify();
    }

    fn cancel_build(&mut self, cx: &mut Context<Self>) {
        if let Some(build) = self.build.take() {
            build.canceller.cancel();
            // Keep the spec in sync with what's displayed.
            if let Some(view) = self.grid.read(cx).view() {
                self.spec = view.spec.clone();
            }
            cx.notify();
        }
    }

    #[cfg(test)]
    pub fn spec(&self) -> &ViewSpec {
        &self.spec
    }

    pub fn add_filter(&mut self, filter: Filter, window: &mut Window, cx: &mut Context<Self>) {
        let mut spec = self.spec.clone();
        spec.filters.retain(|f| f != &filter);
        spec.filters.push(filter);
        self.apply_spec(spec, window, cx);
    }

    fn remove_filter(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let mut spec = self.spec.clone();
        if ix < spec.filters.len() {
            spec.filters.remove(ix);
            self.apply_spec(spec, window, cx);
        }
    }

    pub fn set_where(&mut self, sql: String, window: &mut Window, cx: &mut Context<Self>) {
        let mut spec = self.spec.clone();
        spec.where_sql = sql;
        self.apply_spec(spec, window, cx);
    }

    fn clear_filters(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut spec = self.spec.clone();
        spec.filters.clear();
        spec.where_sql.clear();
        spec.search.clear();
        self.search.update(cx, |s, cx| s.set_value("", window, cx));
        self.apply_spec(spec, window, cx);
    }

    /// Sort by a column. `None` removes the column from the sort. With `add`, the
    /// column becomes an additional (secondary) key.
    pub fn sort_by(&mut self, column: &str, descending: Option<bool>, add: bool, window: &mut Window, cx: &mut Context<Self>) {
        let mut spec = self.spec.clone();
        match descending {
            None => spec.sort.retain(|k| k.column != column),
            Some(descending) => {
                let key = SortKey { column: column.to_string(), descending };
                if add {
                    if let Some(existing) = spec.sort.iter_mut().find(|k| k.column == column) {
                        *existing = key;
                    } else {
                        spec.sort.push(key);
                    }
                } else {
                    spec.sort = vec![key];
                }
            }
        }
        self.apply_spec(spec, window, cx);
    }

    fn apply_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.search.read(cx).value().to_string();
        let mut spec = self.spec.clone();
        spec.search = text.trim().to_string();
        self.apply_spec(spec, window, cx);
    }

    /// Filter by the value of the active cell.
    fn filter_by_cell(&mut self, op: FilterOp, window: &mut Window, cx: &mut Context<Self>) {
        let Some((row, column)) = self.grid.read(cx).active_cell() else { return };
        let Some(view) = self.grid.read(cx).view().cloned() else { return };
        let info = self.dataset.columns[column].clone();
        let job = view.fetch_values(row..row + 1, vec![column], 1);
        cx.spawn_in(window, async move |this, cx| {
            let Ok(values) = job.await else { return };
            let value = values.first().and_then(|r| r.first().cloned()).flatten();
            let _ = this.update_in(cx, |this, window, cx| {
                let filter = match (value, op) {
                    (None, FilterOp::Equals) => Filter::new(&info.name, FilterOp::IsNull, ""),
                    (None, _) => Filter::new(&info.name, FilterOp::IsNotNull, ""),
                    (Some(v), op) if info.kind.is_nested() || info.kind == ColumnKind::Binary => {
                        // Nested values compare as text.
                        Filter::new(&info.name, if op == FilterOp::Equals { FilterOp::Equals } else { FilterOp::NotEquals }, v)
                    }
                    (Some(v), op) => Filter::new(&info.name, op, v),
                };
                this.add_filter(filter, window, cx);
            });
        })
        .detach();
    }

    // ------------------------------------------------------------ events

    fn on_grid_event(&mut self, _: &Entity<GridState>, event: &GridEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            GridEvent::HeaderClicked { column, anchor } => self.open_header_menu(*column, *anchor, window, cx),
            GridEvent::InspectCell { .. } => {
                self.inspector_open = true;
                self.inspector.update(cx, |i, cx| i.refresh(cx));
                cx.notify();
            }
            GridEvent::SelectionChanged => {
                if self.inspector_open {
                    self.inspector.update(cx, |i, cx| i.refresh(cx));
                }
                cx.notify();
            }
            GridEvent::Copied { rows, truncated } => {
                let message = if *truncated {
                    format!("Copied the first {}", format::plural(*rows as u64, "row", "rows"))
                } else if *rows > 1 {
                    format!("Copied {}", format::plural(*rows as u64, "row", "rows"))
                } else {
                    return;
                };
                window.push_notification(message, cx);
            }
            GridEvent::Error(message) => {
                window.push_notification(
                    gpui_kit::component::notification::Notification::error(message.clone()).title("Couldn’t load rows"),
                    cx,
                );
            }
        }
    }

    fn on_search_event(&mut self, state: &Entity<InputState>, event: &InputEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event {
            InputEvent::PressEnter { .. } => self.apply_search(window, cx),
            InputEvent::Change
                // Clearing the box removes the search immediately.
                if state.read(cx).value().trim().is_empty() && !self.spec.search.is_empty() => {
                    self.apply_search(window, cx);
                }
            _ => {}
        }
    }

    fn open_header_menu(&mut self, column: usize, anchor: Bounds<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let doc = cx.entity().downgrade();
        let info = self.dataset.columns[column].clone();
        let sort = self.spec.sort_for(&info.name).cloned();
        let pinned = self.grid.read(cx).is_pinned(column);
        let name = info.name.clone();
        let menu = PopupMenu::build(window, cx, move |menu, _window, _cx| {
            let on = |doc: &WeakEntity<DatasetDocument>, f: fn(&mut DatasetDocument, &str, &mut Window, &mut Context<DatasetDocument>)| {
                let doc = doc.clone();
                let name = name.clone();
                move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                    let _ = doc.update(cx, |this, cx| f(this, &name, window, cx));
                }
            };
            menu.min_w(px(220.))
                .label(info.name.clone())
                .item(
                    PopupMenuItem::new("Sort Ascending")
                        .icon(IconName::SortAscending)
                        .checked(sort.as_ref().is_some_and(|k| !k.descending))
                        .on_click(on(&doc, |this, name, window, cx| this.sort_by(name, Some(false), false, window, cx))),
                )
                .item(
                    PopupMenuItem::new("Sort Descending")
                        .icon(IconName::SortDescending)
                        .checked(sort.as_ref().is_some_and(|k| k.descending))
                        .on_click(on(&doc, |this, name, window, cx| this.sort_by(name, Some(true), false, window, cx))),
                )
                .item(
                    PopupMenuItem::new("Add as Secondary Sort")
                        .disabled(sort.is_some())
                        .on_click(on(&doc, |this, name, window, cx| this.sort_by(name, Some(false), true, window, cx))),
                )
                .item(
                    PopupMenuItem::new("Clear Sort")
                        .disabled(sort.is_none())
                        .on_click(on(&doc, |this, name, window, cx| this.sort_by(name, None, false, window, cx))),
                )
                .separator()
                .item(
                    PopupMenuItem::new("Filter…")
                        .icon(Icon::new(Lucide::Funnel))
                        .on_click(on(&doc, |this, name, window, cx| {
                            let column = this.dataset.column_index(name);
                            this.open_filter_dialog(column, window, cx)
                        })),
                )
                .item(
                    PopupMenuItem::new("Show Only Nulls")
                        .on_click(on(&doc, |this, name, window, cx| this.add_filter(Filter::new(name, FilterOp::IsNull, ""), window, cx))),
                )
                .item(
                    PopupMenuItem::new("Hide Nulls")
                        .on_click(on(&doc, |this, name, window, cx| this.add_filter(Filter::new(name, FilterOp::IsNotNull, ""), window, cx))),
                )
                .separator()
                .item(
                    PopupMenuItem::new(if pinned { "Unpin Column" } else { "Pin Column" })
                        .on_click(on(&doc, |this, name, _, cx| {
                            if let Some(ix) = this.dataset.column_index(name) {
                                this.grid.update(cx, |g, cx| {
                                    let pinned = g.is_pinned(ix);
                                    g.set_pinned(ix, !pinned, cx)
                                });
                            }
                        })),
                )
                .item(
                    PopupMenuItem::new("Hide Column")
                        .icon(IconName::EyeOff)
                        .on_click(on(&doc, |this, name, _, cx| {
                            if let Some(ix) = this.dataset.column_index(name) {
                                this.grid.update(cx, |g, cx| g.set_hidden(ix, true, cx));
                            }
                        })),
                )
                .item(
                    PopupMenuItem::new("Fit Width to Contents")
                        .on_click(on(&doc, |this, name, _, cx| {
                            if let Some(ix) = this.dataset.column_index(name) {
                                this.grid.update(cx, |g, cx| g.autofit_column(ix, cx));
                            }
                        })),
                )
                .separator()
                .item(
                    PopupMenuItem::new("Column Details")
                        .icon(IconName::Info)
                        .on_click(on(&doc, |this, name, _, cx| {
                            if let Some(ix) = this.dataset.column_index(name) {
                                this.show_column_details(ix, cx);
                            }
                        })),
                )
                .item(
                    PopupMenuItem::new("Copy Column Name")
                        .icon(IconName::Copy)
                        .on_click(on(&doc, |_, name, _, cx| cx.write_to_clipboard(ClipboardItem::new_string(name.to_string())))),
                )
        });
        menu.read(cx).focus_handle(cx).focus(window, cx);
        let dismiss = cx.subscribe_in(&menu, window, |this, _, _: &DismissEvent, window, cx| {
            this.header_menu = None;
            // Give keyboard focus back to the grid (the menu had it).
            if this.tab == DocTab::Data {
                this.focus_own_grid(window, cx);
            }
            cx.notify();
        });
        self.header_menu = Some(HeaderMenu {
            menu,
            anchor,
            _dismiss: dismiss,
        });
        cx.notify();
    }

    fn show_column_details(&mut self, column: usize, cx: &mut Context<Self>) {
        self.inspector_open = true;
        self.inspector.update(cx, |i, cx| i.show_column(column, cx));
        cx.notify();
    }

    fn grid_context_menu(&self, cx: &mut Context<Self>) -> impl Fn(PopupMenu, Option<Hit>, &mut Window, &mut Context<PopupMenu>) -> PopupMenu + 'static {
        let doc = cx.entity().downgrade();
        let grid = self.grid.clone();
        move |menu, hit, _window, _cx| {
            let Some(hit) = hit else { return menu };
            let copy = |format: CopyFormat, headers: bool| {
                let grid = grid.clone();
                move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                    grid.update(cx, |g, cx| g.copy_selection(format, headers, cx));
                }
            };
            let filter = |op: FilterOp| {
                let doc = doc.clone();
                move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
                    let _ = doc.update(cx, |this, cx| this.filter_by_cell(op, window, cx));
                }
            };
            match hit {
                Hit::Cell(_) | Hit::RowHeader(_) => {
                    let inspect = {
                        let doc = doc.clone();
                        move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                            let _ = doc.update(cx, |this, cx| {
                                this.inspector_open = true;
                                this.inspector.update(cx, |i, cx| i.refresh(cx));
                                cx.notify();
                            });
                        }
                    };
                    let menu = menu
                        .min_w(px(220.))
                        .item(PopupMenuItem::new("Copy").icon(IconName::Copy).on_click(copy(CopyFormat::Tsv, false)))
                        .item(PopupMenuItem::new("Copy with Headers").on_click(copy(CopyFormat::Tsv, true)))
                        .item(PopupMenuItem::new("Copy as CSV").on_click(copy(CopyFormat::Csv, true)))
                        .item(PopupMenuItem::new("Copy as JSON").on_click(copy(CopyFormat::Json, false)))
                        .item(PopupMenuItem::new("Copy as Markdown").on_click(copy(CopyFormat::Markdown, true)))
                        .item(PopupMenuItem::new("Copy as SQL IN List").on_click(copy(CopyFormat::SqlInList, false)));
                    if matches!(hit, Hit::Cell(_)) {
                        menu.separator()
                            .item(PopupMenuItem::new("Filter to This Value").icon(Icon::new(Lucide::Funnel)).on_click(filter(FilterOp::Equals)))
                            .item(PopupMenuItem::new("Exclude This Value").on_click(filter(FilterOp::NotEquals)))
                            .separator()
                            .item(PopupMenuItem::new("Inspect Value").icon(IconName::Search).on_click(inspect))
                    } else {
                        menu
                    }
                }
                Hit::Header(_) | Hit::HeaderChart(..) => menu
                    .item(PopupMenuItem::new("Copy Column").icon(IconName::Copy).on_click(copy(CopyFormat::Tsv, false)))
                    .item(PopupMenuItem::new("Copy Column with Header").on_click(copy(CopyFormat::Tsv, true))),
                _ => menu,
            }
        }
    }

    // ------------------------------------------------------------ dialogs

    pub fn open_filter_dialog(&mut self, column: Option<usize>, window: &mut Window, cx: &mut Context<Self>) {
        let doc = cx.entity();
        let columns = self.dataset.columns.clone();
        let where_sql = self.spec.where_sql.clone();
        crate::dialogs::filter::open(doc, columns, where_sql, column, window, cx);
    }

    fn open_goto_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let grid = self.grid.clone();
        crate::dialogs::goto_row::open(grid, window, cx);
    }

    fn open_export_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(view) = self.grid.read(cx).view().cloned() else { return };
        let visible: Vec<usize> = self.grid.read(cx).display_columns().to_vec();
        crate::dialogs::export::open(view, visible, self.title.to_string(), window, cx);
    }

    fn set_tab(&mut self, tab: DocTab, window: &mut Window, cx: &mut Context<Self>) {
        self.tab = tab;
        match tab {
            DocTab::Columns if self.columns_tab.is_none() => {
                let tab = cx.new(|cx| ColumnsTab::new(self.grid.clone(), cx));
                self._subscriptions.push(cx.subscribe_in(&tab, window, |this, _, event: &RevealColumn, window, cx| {
                    this.tab = DocTab::Data;
                    this.grid.update(cx, |g, cx| g.reveal_column(event.0, cx));
                    this.focus_own_grid(window, cx);
                    cx.notify();
                }));
                self.columns_tab = Some(tab);
            }
            DocTab::Metadata if self.metadata_tab.is_none() => {
                let dataset = self.dataset.clone();
                self.metadata_tab = Some(cx.new(|cx| MetadataTab::new(dataset, window, cx)));
            }
            DocTab::Sql if self.sql.is_none() => {
                let dataset = self.dataset.clone();
                let panel = cx.new(|cx| SqlPanel::new(Some(dataset), window, cx));
                self._subscriptions.push(cx.subscribe_in(&panel, window, |_, _, event: &SqlEvent, _, cx| match event {
                    SqlEvent::OpenResult { dataset, title } => cx.emit(DocumentEvent::OpenDataset {
                        dataset: dataset.clone(),
                        title: title.clone(),
                    }),
                }));
                self.sql = Some(panel);
            }
            _ => {}
        }
        match tab {
            DocTab::Sql => {
                if let Some(sql) = self.sql.clone() {
                    SqlPanel::focus_editor(&sql, window, cx);
                }
            }
            DocTab::Data => self.focus_own_grid(window, cx),
            // The grid leaves the screen: keep focus in the document so its
            // shortcuts (⌘1–4, ⌘F, ⇧⌘E… or Ctrl) still work.
            DocTab::Columns | DocTab::Metadata => window.focus(&self.focus_handle, cx),
        }
        cx.notify();
    }

    // ------------------------------------------------------------ render

    fn render_toolbar(&self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let ds = &self.dataset;
        let mut facts: Vec<String> = Vec::new();
        facts.push(format::plural(ds.row_count, "row", "rows"));
        facts.push(format::plural(ds.columns.len() as u64, "column", "columns"));
        if let Some(bytes) = ds.total_bytes {
            facts.push(format::bytes(bytes));
        }
        if ds.files.len() > 1 {
            facts.push(format::plural(ds.files.len() as u64, "file", "files"));
        }
        if let Some(format) = ds.format {
            let mut label = format.label().to_string();
            if let Some(pq) = &ds.parquet {
                if !pq.compression.is_empty() {
                    label.push_str(&format!(" · {}", pq.compression.join(", ")));
                }
                facts.push(label);
                facts.push(format::plural(pq.row_groups, "row group", "row groups"));
            } else {
                facts.push(label);
            }
        }
        let location = ds
            .source()
            .map(|s| format::display_path(&s.location))
            .unwrap_or_else(|| self.title.to_string());
        h_flex()
            .h(rems(3.))
            .px_3()
            .gap_3()
            .flex_shrink_0()
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.background)
            .child(
                Icon::new(if ds.is_remote() { Lucide::Cloud } else { Lucide::Sheet })
                    .text_color(theme.muted_foreground),
            )
            .child(
                v_flex()
                    .min_w_0()
                    .flex_1()
                    .child(
                        div()
                            .id("doc-location")
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .truncate()
                            .child(location.clone())
                            .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(location.clone()).build(window, cx)),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .truncate()
                            .child(facts.join("  ·  ")),
                    ),
            )
            .child(
                div().w(rems(18.)).child(
                    Input::new(&self.search)
                        .prefix(Icon::new(IconName::Search).small().text_color(theme.muted_foreground))
                        .cleanable(true)
                        .small(),
                ),
            )
            .child(
                Button::new("add-filter")
                    .icon(Icon::new(Lucide::ListFilter))
                    .label("Filter")
                    .small()
                    .outline()
                    .tooltip_with_action("Add filter", &AddFilter, None)
                    .on_click(cx.listener(|this, _, window, cx| this.open_filter_dialog(None, window, cx))),
            )
            .child(
                Button::new("export")
                    .icon(Icon::new(Lucide::Download))
                    .small()
                    .ghost()
                    .tooltip_with_action("Export", &Export, None)
                    .on_click(cx.listener(|this, _, window, cx| this.open_export_dialog(window, cx))),
            )
            .child({
                let path = ds.source().filter(|s| !s.is_remote()).map(|s| s.location.clone());
                Button::new("more")
                    .icon(IconName::Ellipsis)
                    .small()
                    .ghost()
                    .dropdown_menu(move |menu, _, _| {
                        let path = path.clone();
                        menu.menu("Go to Row…", Box::new(GoToRow))
                            .menu("Compare with…", Box::new(Compare))
                            .menu("Reload", Box::new(Reload))
                            .separator()
                            .menu("Toggle Column Summaries", Box::new(ToggleSummaries))
                            .menu("Compute Exact Summaries", Box::new(ExactSummaries))
                            .separator()
                            .item(PopupMenuItem::new(REVEAL_IN_FILE_MANAGER).disabled(path.is_none()).on_click({
                                let path = path.clone();
                                move |_, _, cx| {
                                    if let Some(p) = &path {
                                        cx.reveal_path(std::path::Path::new(p));
                                    }
                                }
                            }))
                            .item(PopupMenuItem::new("Copy Path").disabled(path.is_none()).on_click(move |_, _, cx| {
                                if let Some(p) = &path {
                                    cx.write_to_clipboard(ClipboardItem::new_string(p.clone()));
                                }
                            }))
                    })
            })
            .into_any_element()
            .into_element()
    }

    fn render_filter_bar(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let spec = self.spec.clone();
        if !spec.has_filter() {
            return None;
        }
        let theme = cx.theme().clone();
        let mut chips: Vec<AnyElement> = Vec::new();
        let chip = |id: SharedString, text: String, on_remove: RemoveChip, cx: &mut Context<Self>| {
            let theme = cx.theme();
            h_flex()
                .id(id.clone())
                .gap_1()
                .pl_2()
                .pr_1()
                .h(rems(1.5))
                .rounded(theme.radius)
                .bg(theme.secondary)
                .border_1()
                .border_color(theme.border)
                .text_xs()
                .child(div().max_w(rems(24.)).truncate().child(text))
                .child(
                    Button::new(SharedString::from(format!("{id}-x")))
                        .icon(Icon::new(Lucide::X))
                        .xsmall()
                        .ghost()
                        .on_click(cx.listener(move |this, _, window, cx| on_remove(this, window, cx))),
                )
                .into_any_element()
        };
        if !spec.search.is_empty() {
            chips.push(chip(
                "chip-search".into(),
                format!("contains “{}”", spec.search),
                Box::new(|this, window, cx| {
                    this.search.update(cx, |s, cx| s.set_value("", window, cx));
                    this.apply_search(window, cx);
                }),
                cx,
            ));
        }
        for (ix, filter) in spec.filters.iter().enumerate() {
            chips.push(chip(
                SharedString::from(format!("chip-filter-{ix}")),
                filter.describe(),
                Box::new(move |this, window, cx| this.remove_filter(ix, window, cx)),
                cx,
            ));
        }
        if !spec.where_sql.is_empty() {
            chips.push(chip(
                "chip-where".into(),
                format!("WHERE {}", spec.where_sql),
                Box::new(|this, window, cx| this.set_where(String::new(), window, cx)),
                cx,
            ));
        }
        let shown = self.grid.read(cx).row_count();
        Some(
            h_flex()
                .px_3()
                .py_1p5()
                .gap_2()
                .flex_wrap()
                .flex_shrink_0()
                .border_b_1()
                .border_color(theme.border)
                .bg(theme.background)
                .child(Icon::new(Lucide::ListFilter).small().text_color(theme.muted_foreground))
                .children(chips)
                .child(
                    Button::new("clear-filters")
                        .label("Clear")
                        .xsmall()
                        .ghost()
                        .on_click(cx.listener(|this, _, window, cx| this.clear_filters(window, cx))),
                )
                .child(div().flex_1())
                .when(self.build.is_none(), |this| {
                    this.child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(format!("{} of {}", format::count(shown), format::plural(self.dataset.row_count, "row", "rows"))),
                    )
                })
                .into_any_element(),
        )
    }

    fn render_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let selected = DocTab::ALL.iter().position(|t| *t == self.tab).unwrap_or(0);
        let show_summaries = self.grid.read(cx).show_summaries();
        h_flex()
            .px_2()
            .flex_shrink_0()
            .justify_between()
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.background)
            .child(
                TabBar::new("doc-tabs")
                    .underline()
                    .small()
                    .selected_index(selected)
                    .on_click(cx.listener(|this, ix: &usize, window, cx| this.set_tab(DocTab::ALL[*ix], window, cx)))
                    .children(DocTab::ALL.iter().map(|t| t.label())),
            )
            .child(
                h_flex()
                    .gap_1()
                    .when(self.tab == DocTab::Data, |this| {
                        this.child(
                            Button::new("toggle-summaries")
                                .icon(Icon::new(Lucide::ChartColumn))
                                .xsmall()
                                .ghost()
                                .selected(show_summaries)
                                .tooltip_with_action("Column summaries", &ToggleSummaries, None)
                                .on_click(cx.listener(|this, _, _, cx| this.toggle_summaries(cx))),
                        )
                    })
                    .child(
                        Button::new("toggle-inspector")
                            .icon(IconName::PanelRight)
                            .xsmall()
                            .ghost()
                            .selected(self.inspector_open)
                            .tooltip_with_action("Inspector", &ToggleInspector, None)
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.inspector_open = !this.inspector_open;
                                this.inspector.update(cx, |i, cx| i.refresh(cx));
                                cx.notify();
                            })),
                    ),
            )
    }

    fn toggle_summaries(&mut self, cx: &mut Context<Self>) {
        let show = !self.grid.read(cx).show_summaries();
        self.grid.update(cx, |g, cx| g.set_show_summaries(show, cx));
        AppState::update_settings(cx, |s| s.show_summaries = show);
    }

    fn render_status(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let grid = self.grid.read(cx);
        let view = grid.view();
        let mut left: Vec<AnyElement> = Vec::new();
        if let Some(build) = &self.build {
            let elapsed = build.started.elapsed();
            left.push(
                h_flex()
                    .gap_2()
                    .child(Spinner::new().xsmall())
                    .child(format!("{} {}…", build.label, format::plural(self.dataset.row_count, "row", "rows")))
                    .when(elapsed > Duration::from_millis(500), |this| {
                        this.child(format::duration_ms(elapsed.as_millis() as u64))
                    })
                    .child(
                        Button::new("cancel-build")
                            .label("Cancel")
                            .xsmall()
                            .ghost()
                            .on_click(cx.listener(|this, _, _, cx| this.cancel_build(cx))),
                    )
                    .into_any_element(),
            );
        } else if let Some(view) = view {
            let mut text = format::plural(view.row_count, "row", "rows").to_string();
            if !view.is_identity() {
                text.push_str(&format!(" of {}", format::count(self.dataset.row_count)));
                if view.build_millis > 0 {
                    text.push_str(&format!(" · {}", format::duration_ms(view.build_millis)));
                }
            }
            if view.truncated {
                text.push_str(" · result limited");
            }
            left.push(div().child(text).into_any_element());
        }
        if let Some(error) = &self.error {
            left.push(div().text_color(theme.danger).truncate().child(error.clone()).into_any_element());
        }
        let mut right: Vec<AnyElement> = Vec::new();
        if grid.is_loading() {
            right.push(h_flex().gap_1().child(Spinner::new().xsmall()).child("Loading rows").into_any_element());
        }
        if grid.is_summarizing() {
            right.push(div().child("Summarizing…").into_any_element());
        }
        if let Some(selection) = grid.selection() {
            let rows = grid.row_count();
            let count = selection.cell_count(rows, grid.display_columns().len());
            let text = if selection.is_single_cell() {
                let column = grid
                    .display_columns()
                    .get(selection.head.col)
                    .and_then(|c| self.dataset.columns.get(*c))
                    .map(|c| c.name.clone())
                    .unwrap_or_default();
                format!("Row {} · {}", format::count(selection.head.row), column)
            } else {
                let rows = selection.rows(rows);
                format!(
                    "{} × {} selected ({} cells)",
                    format::plural(rows.end - rows.start, "row", "rows"),
                    format::plural(selection.columns(grid.display_columns().len()).len() as u64, "column", "columns"),
                    format::count(count)
                )
            };
            right.push(div().child(text).into_any_element());
        }
        let hidden = self.dataset.columns.len() - grid.display_columns().len();
        if hidden > 0 {
            right.push(
                Button::new("show-hidden")
                    .label(format!("{hidden} hidden"))
                    .xsmall()
                    .ghost()
                    .tooltip("Show all columns")
                    .on_click(cx.listener(|this, _, _, cx| this.grid.update(cx, |g, cx| g.show_all_columns(cx))))
                    .into_any_element(),
            );
        }
        for note in &self.dataset.notes {
            right.push(div().text_color(theme.warning).child(note.clone()).into_any_element());
        }
        h_flex()
            .h(rems(1.75))
            .px_3()
            .gap_4()
            .flex_shrink_0()
            .justify_between()
            .border_t_1()
            .border_color(theme.border)
            .bg(theme.background)
            .text_xs()
            .text_color(theme.muted_foreground)
            .child(h_flex().gap_3().min_w_0().children(left))
            .child(h_flex().gap_3().children(right))
    }

    fn render_body(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        match self.tab {
            DocTab::Data => {
                let menu = self.grid_context_menu(cx);
                let grid = Grid::new(&self.grid).context_menu(menu);
                let inspector = self.inspector.clone();
                h_resizable("doc-split")
                    .child(resizable_panel().child(div().size_full().child(grid)))
                    .child(
                        resizable_panel()
                            .visible(self.inspector_open)
                            .size(px(340.))
                            .size_range(px(240.)..px(900.))
                            .child(inspector),
                    )
                    .into_any_element()
            }
            DocTab::Columns => self
                .columns_tab
                .clone()
                .map(|t| t.into_any_element())
                .unwrap_or_else(|| div().into_any_element()),
            DocTab::Metadata => self
                .metadata_tab
                .clone()
                .map(|t| t.into_any_element())
                .unwrap_or_else(|| div().into_any_element()),
            DocTab::Sql => {
                let _ = window;
                self.sql.clone().map(|t| t.into_any_element()).unwrap_or_else(|| div().into_any_element())
            }
        }
    }
}

impl Render for DatasetDocument {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let header_menu = self.header_menu.as_ref().map(|m| {
            deferred(
                anchored()
                    .position(point(m.anchor.origin.x, m.anchor.bottom()))
                    .snap_to_window_with_margin(px(8.))
                    .child(m.menu.clone()),
            )
            .with_priority(1)
        });
        let body = self.render_body(window, cx);
        v_flex()
            .id("dataset-document")
            .key_context("DatasetDocument")
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(theme.background)
            .on_action(cx.listener(|this, _: &Find, window, cx| {
                this.tab = DocTab::Data;
                this.search.update(cx, |s, cx| s.focus(window, cx));
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &AddFilter, window, cx| this.open_filter_dialog(None, window, cx)))
            .on_action(cx.listener(|this, _: &ClearFilters, window, cx| this.clear_filters(window, cx)))
            .on_action(cx.listener(|this, _: &GoToRow, window, cx| this.open_goto_dialog(window, cx)))
            .on_action(cx.listener(|this, _: &Export, window, cx| this.open_export_dialog(window, cx)))
            .on_action(cx.listener(|_, _: &Compare, _, cx| cx.emit(DocumentEvent::Compare)))
            .on_action(cx.listener(|this, _: &ShowData, window, cx| this.set_tab(DocTab::Data, window, cx)))
            .on_action(cx.listener(|this, _: &ShowColumns, window, cx| this.set_tab(DocTab::Columns, window, cx)))
            .on_action(cx.listener(|this, _: &ShowMetadata, window, cx| this.set_tab(DocTab::Metadata, window, cx)))
            .on_action(cx.listener(|this, _: &ShowSql, window, cx| this.set_tab(DocTab::Sql, window, cx)))
            .on_action(cx.listener(|this, _: &ToggleInspector, _, cx| {
                this.inspector_open = !this.inspector_open;
                this.inspector.update(cx, |i, cx| i.refresh(cx));
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &ToggleSummaries, _, cx| this.toggle_summaries(cx)))
            .on_action(cx.listener(|this, _: &ExactSummaries, window, cx| {
                this.grid.update(cx, |g, cx| g.recompute_summaries(StatsMode::Exact, cx));
                window.push_notification("Computing exact summaries over all rows…", cx);
            }))
            .child(self.render_toolbar(window, cx))
            .children(self.render_filter_bar(cx))
            .child(self.render_tabs(cx))
            .child(div().flex_1().min_h_0().child(body))
            .child(self.render_status(cx))
            .children(header_menu)
    }
}
