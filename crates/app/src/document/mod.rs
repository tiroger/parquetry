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
    Canceller, CodeFlavor, CodeOptions, ColumnKind, CopyFormat, Dataset, Filter, FilterOp, SelectionStats, SortKey, StatsMode, View, ViewSpec, python_packages, view_code,
};
use parquetry_grid::{Grid, GridEvent, GridState, Hit, Selection, SummaryState};

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

    /// Stable name for saved sessions.
    fn key(self) -> &'static str {
        match self {
            DocTab::Data => "data",
            DocTab::Columns => "columns",
            DocTab::Metadata => "metadata",
            DocTab::Sql => "sql",
        }
    }

    fn from_key(key: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.key() == key)
    }

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
    /// Row to scroll to once the view being built is shown (session restore).
    pending_top: Option<u64>,
    /// Totals of the current multi-cell selection, for the status bar; `None`
    /// inside means too costly to compute.
    selection_stats: Option<(Selection, Option<SelectionStats>)>,
    stats_task: Option<Task<()>>,
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
            pending_top: None,
            selection_stats: None,
            stats_task: None,
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
                        let top = this.pending_top.take();
                        this.grid.update(cx, |g, cx| {
                            g.set_view(Some(view), false, cx);
                            if let Some(top) = top {
                                g.scroll_to_row(top, cx);
                            }
                        });
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

    /// What to save to reopen this tab later; `None` for data without a location
    /// (query results).
    pub fn session(&self, cx: &App) -> Option<crate::session::DatasetSession> {
        let source = self.dataset.source()?;
        let grid = self.grid.read(cx);
        Some(crate::session::DatasetSession {
            location: source.location.clone(),
            format: source.format,
            view: self.spec.clone(),
            layout: grid.column_arrangement(),
            tab: self.tab.key().to_string(),
            top_row: grid.top_row(),
            inspector: self.inspector_open,
        })
    }

    /// Put back a saved state. Filters and sort keys on columns that no longer
    /// exist are dropped.
    pub fn restore(&mut self, state: &crate::session::DatasetSession, window: &mut Window, cx: &mut Context<Self>) {
        self.restore_layout(&state.layout, cx);
        let known = |name: &str| self.dataset.column_index(name).is_some();
        let mut spec = state.view.clone();
        spec.filters.retain(|f| known(&f.column));
        spec.sort.retain(|k| known(&k.column));
        if !spec.search.is_empty() {
            let search = spec.search.clone();
            self.search.update(cx, |input, cx| input.set_value(search, window, cx));
        }
        self.inspector_open = state.inspector;
        if let Some(tab) = DocTab::from_key(&state.tab) {
            self.set_tab(tab, window, cx);
        }
        if spec.is_identity() {
            self.grid.update(cx, |g, cx| g.scroll_to_row(state.top_row, cx));
        } else {
            self.pending_top = Some(state.top_row);
            self.apply_spec(spec, window, cx);
        }
        cx.notify();
    }

    /// Arrange columns as they were the last time this data was open.
    pub fn restore_layout(&mut self, layout: &parquetry_grid::ColumnArrangement, cx: &mut Context<Self>) {
        self.grid.update(cx, |g, cx| g.apply_column_arrangement(layout, cx));
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

    // ------------------------------------------------------------ code and notebooks

    /// How the view's code should look: the shown columns (when they differ from
    /// the file's), and the S3 profile to name.
    fn code_options(&self, cx: &App) -> CodeOptions {
        let grid = self.grid.read(cx);
        let display = grid.display_columns();
        let natural = display.len() == self.dataset.columns.len() && display.iter().enumerate().all(|(i, c)| i == *c);
        let columns = if natural {
            Vec::new()
        } else {
            display.iter().filter_map(|&c| self.dataset.columns.get(c)).map(|c| c.name.clone()).collect()
        };
        let s3 = &AppState::settings(cx).engine.s3;
        CodeOptions { columns, aws_profile: s3.profile.clone(), aws_region: s3.region.clone(), preview_rows: None }
    }

    /// Put the view on the clipboard as SQL, Polars or pandas code.
    fn copy_code(&mut self, flavor: CodeFlavor, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_kit::component::notification::Notification;
        match view_code(&self.dataset, &self.spec, flavor, &self.code_options(cx)) {
            Ok(code) => {
                cx.write_to_clipboard(ClipboardItem::new_string(code));
                window.push_notification(Notification::success(format!("Copied the view as {}", flavor.label())), cx);
            }
            Err(error) => {
                window.push_notification(Notification::error(error.to_string()).title("Couldn’t write code for this view"), cx);
            }
        }
    }

    /// Write the view as a marimo notebook and open it.
    fn open_in_marimo(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        use gpui_kit::component::notification::Notification;
        let settings = AppState::settings(cx).clone();
        let flavor = match settings.notebook_library {
            crate::settings::NotebookLibrary::Polars => CodeFlavor::Polars,
            crate::settings::NotebookLibrary::Pandas => CodeFlavor::Pandas,
        };
        let rows = self.grid.read(cx).row_count();
        // Big views stay lazy; `df` gets the first rows.
        let options = CodeOptions { preview_rows: (rows > 1_000_000).then_some(100_000), ..self.code_options(cx) };
        let code = match view_code(&self.dataset, &self.spec, flavor, &options) {
            Ok(code) => code,
            Err(error) => {
                window.push_notification(Notification::error(error.to_string()).title("Couldn’t write a notebook for this view"), cx);
                return;
            }
        };
        let mut summary = format::plural(rows, "row", "rows");
        if rows != self.dataset.row_count {
            summary.push_str(&format!(" of {}", format::count(self.dataset.row_count)));
        }
        let described: Vec<String> = self.spec.filters.iter().map(|f| f.describe()).collect();
        if !described.is_empty() {
            summary.push_str(&format!(" · {}", described.join(" · ")));
        }
        let packages = python_packages(&code);
        let notebook = crate::notebook::marimo_notebook(&self.title, &summary, &code, &packages);
        let dir = settings.notebooks_folder();
        let file = crate::notebook::notebook_file_name(&self.title);
        let path = match crate::notebook::write_notebook(&dir, &file, &notebook) {
            Ok(path) => path,
            Err(error) => {
                window.push_notification(Notification::error(format!("{error:#}")).title("Couldn’t save the notebook"), cx);
                return;
            }
        };
        // Opened after this update (a notebook window builds views of its own).
        cx.defer(move |cx| {
            if let Err(error) = crate::notebook::show_notebook(path, cx) {
                crate::notebook::notify_error(cx, "Couldn’t open the notebook", &format!("{error:#}"));
            }
        });
    }

    /// Total the selected cells in the background (after a short pause, so dragging
    /// a selection doesn't run a query per step). Replacing the task cancels the last.
    fn refresh_selection_stats(&mut self, cx: &mut Context<Self>) {
        let grid = self.grid.read(cx);
        let selection = grid.selection().filter(|s| !s.is_single_cell());
        let (Some(selection), Some(view)) = (selection, grid.view().cloned()) else {
            self.selection_stats = None;
            self.stats_task = None;
            return;
        };
        if self.selection_stats.as_ref().is_some_and(|(s, _)| *s == selection) {
            return;
        }
        let rows = selection.rows(grid.row_count());
        let display = grid.display_columns();
        let columns: Vec<usize> = selection.columns(display.len()).filter_map(|d| display.get(d).copied()).collect();
        self.stats_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(Duration::from_millis(150)).await;
            let stats = view.selection_stats(rows, columns).await;
            let _ = this.update(cx, |this, cx| {
                if let Ok(stats) = stats {
                    this.selection_stats = Some((selection, stats));
                    cx.notify();
                }
            });
        }));
    }

    /// Replace the column's value filters (=, ≠, one of, not one of, null checks)
    /// with `filters`, from the value counts panel.
    pub fn replace_value_filters(&mut self, column: &str, filters: Vec<Filter>, window: &mut Window, cx: &mut Context<Self>) {
        const VALUE_OPS: [FilterOp; 6] =
            [FilterOp::Equals, FilterOp::NotEquals, FilterOp::In, FilterOp::NotIn, FilterOp::IsNull, FilterOp::IsNotNull];
        let mut spec = self.spec.clone();
        spec.filters.retain(|f| !(f.column == column && VALUE_OPS.contains(&f.op)));
        spec.filters.extend(filters);
        self.apply_spec(spec, window, cx);
    }

    /// Open the value counts of `column` (a dataset column index) in the current view.
    pub fn open_value_counts(&mut self, column: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(view) = self.grid.read(cx).view().cloned() else { return };
        crate::dialogs::value_counts::open(cx.entity(), view, column, window, cx);
    }

    /// Keep the rows behind a header chart bar. Replaces the column's earlier bar
    /// filters, so clicking bars again drills into the (re-summarized) data.
    fn filter_to_chart_bar(&mut self, column: usize, bar: usize, anchor: Bounds<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let filters = match self.grid.read(cx).summary(column) {
            Some(SummaryState::Ready(summary)) => summary.filters_for_bar(bar),
            _ => None,
        };
        let Some(filters) = filters.filter(|f| !f.is_empty()) else {
            self.open_header_menu(column, anchor, window, cx);
            return;
        };
        const BAR_OPS: [FilterOp; 7] = [
            FilterOp::GreaterOrEqual,
            FilterOp::Less,
            FilterOp::LessOrEqual,
            FilterOp::Equals,
            FilterOp::NotIn,
            FilterOp::IsNull,
            FilterOp::IsNotNull,
        ];
        let name = filters[0].column.clone();
        let mut spec = self.spec.clone();
        spec.filters.retain(|f| !(f.column == name && BAR_OPS.contains(&f.op)));
        spec.filters.extend(filters);
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
            GridEvent::ChartBarClicked { column, bar, anchor } => self.filter_to_chart_bar(*column, *bar, *anchor, window, cx),
            GridEvent::InspectCell { .. } => {
                self.inspector_open = true;
                self.inspector.update(cx, |i, cx| i.refresh(cx));
                cx.notify();
            }
            GridEvent::SelectionChanged => {
                self.refresh_selection_stats(cx);
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
                    PopupMenuItem::new("Value Counts…")
                        .icon(Icon::new(Lucide::ChartBarBig))
                        .on_click(on(&doc, |this, name, window, cx| {
                            if let Some(column) = this.dataset.column_index(name) {
                                this.open_value_counts(column, window, cx);
                            }
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

    /// Show the Data tab with `column` (a dataset column index) selected and in view,
    /// un-hiding it if needed.
    pub fn reveal_column(&mut self, column: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.tab = DocTab::Data;
        self.grid.update(cx, |g, cx| {
            if g.is_hidden(column) {
                g.set_hidden(column, false, cx);
            }
            g.reveal_column(column, cx);
        });
        self.focus_own_grid(window, cx);
        cx.notify();
    }

    fn open_goto_column(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        crate::dialogs::goto_column::open(cx.entity(), self.grid.clone(), window, cx);
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
                    this.reveal_column(event.0, window, cx);
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
        // File name large, its folder as a quiet line, the facts as small pills.
        let location = ds.source().map(|s| format::display_path(&s.location));
        let (folder, name) = match ds.source() {
            Some(source) => format::split_location(&source.location),
            None => (String::new(), self.title.to_string()),
        };
        let tooltip = location.unwrap_or_else(|| self.title.to_string());
        let pill = |text: String| {
            div()
                .px_1p5()
                .rounded_full()
                .border_1()
                .border_color(theme.border)
                .bg(theme.secondary)
                .text_color(theme.muted_foreground)
                .whitespace_nowrap()
                .child(text)
        };
        h_flex()
            .px_3()
            .py_2()
            .gap_3()
            .flex_shrink_0()
            .bg(theme.background)
            .child(
                Icon::new(if ds.is_remote() { Lucide::Cloud } else { Lucide::Sheet })
                    .text_color(theme.primary),
            )
            .child(
                v_flex()
                    .min_w_0()
                    .flex_1()
                    .gap_0p5()
                    .child(
                        h_flex()
                            .id("doc-location")
                            .min_w_0()
                            .gap_2()
                            .items_baseline()
                            .child(div().text_base().font_weight(FontWeight::SEMIBOLD).truncate().child(name))
                            .when(!folder.is_empty(), |this| {
                                this.child(div().min_w_0().text_xs().text_color(theme.muted_foreground).truncate().child(folder))
                            })
                            .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(tooltip.clone()).build(window, cx)),
                    )
                    .child(h_flex().gap_1().text_xs().overflow_x_hidden().children(facts.into_iter().map(pill))),
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
                Button::new("open-in")
                    .icon(Icon::new(Lucide::NotebookPen))
                    .label("Open in…")
                    .small()
                    .outline()
                    .dropdown_menu(|menu, _, _| {
                        menu.menu_with_icon("Open in marimo", Icon::new(Lucide::NotebookPen), Box::new(OpenInMarimo))
                            .separator()
                            .label("Copy view as code")
                            .menu("SQL (DuckDB)", Box::new(CopyAsSql))
                            .menu("Polars", Box::new(CopyAsPolars))
                            .menu("pandas", Box::new(CopyAsPandas))
                    }),
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
                            .menu("Go to Column…", Box::new(GoToColumn))
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

    /// The active search, filters, WHERE clause and sort, as removable chips.
    fn filter_chips(&self, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let spec = self.spec.clone();
        let mut chips: Vec<AnyElement> = Vec::new();
        let chip = |id: SharedString, text: String, accent: bool, on_remove: RemoveChip, cx: &mut Context<Self>| {
            let theme = cx.theme();
            let (bg, fg, border) = if accent {
                (theme.primary.opacity(0.13), theme.foreground, theme.primary.opacity(0.4))
            } else {
                (theme.secondary, theme.foreground, theme.border)
            };
            h_flex()
                .id(id.clone())
                .gap_0p5()
                .pl_2()
                .pr_0p5()
                .h(rems(1.5))
                .rounded_full()
                .bg(bg)
                .text_color(fg)
                .border_1()
                .border_color(border)
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
                true,
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
                true,
                Box::new(move |this, window, cx| this.remove_filter(ix, window, cx)),
                cx,
            ));
        }
        if !spec.where_sql.is_empty() {
            chips.push(chip(
                "chip-where".into(),
                format!("WHERE {}", spec.where_sql),
                true,
                Box::new(|this, window, cx| this.set_where(String::new(), window, cx)),
                cx,
            ));
        }
        for (ix, key) in spec.sort.iter().enumerate() {
            let arrow = if key.descending { "↓" } else { "↑" };
            let column = key.column.clone();
            chips.push(chip(
                SharedString::from(format!("chip-sort-{ix}")),
                format!("{arrow} {}", key.column),
                false,
                Box::new(move |this, window, cx| this.sort_by(&column, None, true, window, cx)),
                cx,
            ));
        }
        if spec.has_filter() {
            chips.push(
                Button::new("clear-filters")
                    .label("Clear")
                    .xsmall()
                    .ghost()
                    .on_click(cx.listener(|this, _, window, cx| this.clear_filters(window, cx)))
                    .into_any_element(),
            );
        }
        chips
    }

    fn render_tabs(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let selected = DocTab::ALL.iter().position(|t| *t == self.tab).unwrap_or(0);
        let show_summaries = self.grid.read(cx).show_summaries();
        let border = theme.border;
        let background = theme.background;
        let chips = self.filter_chips(cx);
        h_flex()
            .px_3()
            .pb_2()
            .gap_3()
            .flex_shrink_0()
            .border_b_1()
            .border_color(border)
            .bg(background)
            .child(
                TabBar::new("doc-tabs")
                    .segmented()
                    .small()
                    .selected_index(selected)
                    .on_click(cx.listener(|this, ix: &usize, window, cx| this.set_tab(DocTab::ALL[*ix], window, cx)))
                    .children(DocTab::ALL.iter().map(|t| t.label())),
            )
            .child(h_flex().flex_1().min_w_0().gap_1p5().flex_wrap().children(chips))
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
            if let Some(first) = self.spec.sort.first() {
                let arrow = if first.descending { "↓" } else { "↑" };
                let more = self.spec.sort.len() - 1;
                text.push_str(&format!(" · sorted by {} {arrow}", first.column));
                if more > 0 {
                    text.push_str(&format!(" +{more}"));
                }
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
            // Like a spreadsheet: totals of the selected numbers.
            if let Some((stats_for, stats)) = &self.selection_stats
                && *stats_for == selection
            {
                let strong = |label: &str, value: String| {
                    h_flex()
                        .gap_1()
                        .child(label.to_string())
                        .child(div().text_color(theme.foreground).font_weight(FontWeight::MEDIUM).child(value))
                        .into_any_element()
                };
                match stats {
                    Some(stats) if stats.numbers > 0 => {
                        let n = parquetry_engine::format_number;
                        right.push(strong("sum", n(stats.sum)));
                        right.extend(stats.mean().map(|mean| strong("avg", n(mean))));
                        right.extend(stats.min.map(|min| strong("min", n(min))));
                        right.extend(stats.max.map(|max| strong("max", n(max))));
                    }
                    Some(stats) if stats.values < stats.cells => {
                        right.push(div().child(format!("{} non-null", format::count(stats.values))).into_any_element());
                    }
                    Some(_) => {}
                    None => right.push(div().child("too many rows to total").into_any_element()),
                }
            }
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
            .on_action(cx.listener(|this, _: &GoToColumn, window, cx| this.open_goto_column(window, cx)))
            .on_action(cx.listener(|this, _: &OpenInMarimo, window, cx| this.open_in_marimo(window, cx)))
            .on_action(cx.listener(|this, _: &CopyAsSql, window, cx| this.copy_code(CodeFlavor::Sql, window, cx)))
            .on_action(cx.listener(|this, _: &CopyAsPolars, window, cx| this.copy_code(CodeFlavor::Polars, window, cx)))
            .on_action(cx.listener(|this, _: &CopyAsPandas, window, cx| this.copy_code(CodeFlavor::Pandas, window, cx)))
            .on_action(cx.listener(|this, _: &ShowValueCounts, window, cx| {
                // The selected column, or the first one.
                let grid = this.grid.read(cx);
                let display = grid.selection().map(|s| s.head.col).unwrap_or(0);
                if let Some(&column) = grid.display_columns().get(display) {
                    this.open_value_counts(column, window, cx);
                }
            }))
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
            .child(self.render_tabs(cx))
            .child(div().flex_1().min_h_0().child(body))
            .child(self.render_status(cx))
            .children(header_menu)
    }
}
