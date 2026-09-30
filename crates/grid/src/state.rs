//! `GridState`: the retained model behind a data grid.
//!
//! It owns everything that must survive between frames: which view is shown,
//! the cache of fetched cells, column widths and order, scroll position,
//! selection, and column summaries. The [`crate::element::GridElement`] reads
//! it to paint and feeds pointer input back to it.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::sync::Arc;

use gpui_kit::*;
use parquetry_engine::{
    ColumnInfo, ColumnKind, ColumnSummary, CopyFormat, PageRequest, StatsMode, View, format_cells,
    summarize,
};

use crate::cache::{BlockKey, COL_BLOCK, CellState, PageCache, ROW_BLOCK, SCATTERED_ROW_BLOCK, blocks_for};
use crate::layout::{ColumnLayout, RowViewport, Thumb, offset_for_thumb};
use crate::selection::{CellPos, Movement, Selection, SelectionKind, move_head};

/// Most rows copied to the clipboard at once.
pub const MAX_COPY_ROWS: usize = 100_000;
/// Columns summarized per engine request.
const SUMMARY_BATCH: usize = 8;
/// Concurrent block fetches.
const MAX_INFLIGHT: usize = 16;

/// A column as the grid shows it.
#[derive(Debug, Clone)]
pub struct GridColumn {
    pub info: ColumnInfo,
    /// Width in rem units, so it scales with the app's zoom.
    pub width: f32,
    /// Still sized automatically (not resized by hand).
    pub auto_width: bool,
}

/// Loading state of a column summary.
#[derive(Debug, Clone)]
pub enum SummaryState {
    Loading,
    Ready(Arc<ColumnSummary>),
    Failed(SharedString),
}

/// What sits under a point of the grid.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Hit {
    Cell(CellPos),
    RowHeader(u64),
    /// A column header (displayed column index).
    Header(usize),
    /// The summary chart in a header; the bar/bin index when over one.
    HeaderChart(usize, Option<usize>),
    ResizeHandle(usize),
    Corner,
    VerticalScrollbar { on_thumb: bool },
    HorizontalScrollbar { on_thumb: bool },
}

/// Emitted for the owner to act on.
#[derive(Debug, Clone)]
pub enum GridEvent {
    /// A header was clicked: show the column menu below `anchor`.
    HeaderClicked {
        column: usize,
        anchor: Bounds<Pixels>,
    },
    /// A bar of a header chart was clicked (`bar` indexes the bars as drawn).
    ChartBarClicked {
        column: usize,
        bar: usize,
        anchor: Bounds<Pixels>,
    },
    /// Show the full value of a cell (double-click, Enter).
    InspectCell { row: u64, column: usize },
    SelectionChanged,
    Copied { rows: usize, truncated: bool },
    Error(SharedString),
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum Drag {
    SelectCells,
    SelectRows,
    SelectColumns,
    Resize { col: usize, start_x: f32, start_width: f32 },
    VerticalThumb { grab: f32 },
    HorizontalThumb { grab: f32 },
    /// `bar`: the chart bar under the pointer when the press started.
    HeaderPress { col: usize, start: Point<Pixels>, bar: Option<usize> },
}

/// Sizes derived from the window's rem size and fonts, recomputed each frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Metrics {
    pub rem: f32,
    pub font_size: f32,
    pub small_font_size: f32,
    pub row_height: f32,
    pub header_height: f32,
    pub padding: f32,
    pub char_width: f32,
    pub scrollbar: f32,
}

impl Metrics {
    pub fn new(rem: f32, char_width: f32, show_summaries: bool) -> Self {
        let font_size = (rem * 0.8125).round();
        Self {
            rem,
            font_size,
            small_font_size: (rem * 0.6875).round(),
            row_height: (rem * 1.625).round(),
            header_height: if show_summaries {
                (rem * 6.0).round()
            } else {
                (rem * 2.75).round()
            },
            padding: (rem * 0.5).round(),
            char_width,
            scrollbar: (rem * 0.625).round(),
        }
    }
}

/// Geometry of the last painted frame, used to interpret pointer input.
#[derive(Debug, Clone)]
pub(crate) struct Frame {
    pub bounds: Bounds<Pixels>,
    pub metrics: Metrics,
    pub gutter: f32,
    pub layout: ColumnLayout,
    pub scroll_x: f32,
    pub viewport: RowViewport,
    pub vertical_track: Option<(Bounds<Pixels>, Thumb)>,
    pub horizontal_track: Option<(Bounds<Pixels>, Thumb)>,
}

impl Frame {
    pub fn body_left(&self) -> f32 {
        f32::from(self.bounds.origin.x) + self.gutter
    }

    pub fn body_top(&self) -> f32 {
        f32::from(self.bounds.origin.y) + self.metrics.header_height
    }

    pub fn body_width(&self) -> f32 {
        (f32::from(self.bounds.size.width) - self.gutter).max(0.0)
    }

    pub fn body_height(&self) -> f32 {
        (f32::from(self.bounds.size.height) - self.metrics.header_height).max(0.0)
    }

    /// What's under a window position.
    pub fn hit(&self, position: Point<Pixels>) -> Option<Hit> {
        if !self.bounds.contains(&position) {
            return None;
        }
        if let Some((track, thumb)) = &self.vertical_track
            && track.contains(&position) {
                let y = f32::from(position.y - track.origin.y);
                return Some(Hit::VerticalScrollbar {
                    on_thumb: y >= thumb.start && y <= thumb.start + thumb.length,
                });
            }
        if let Some((track, thumb)) = &self.horizontal_track
            && track.contains(&position) {
                let x = f32::from(position.x - track.origin.x);
                return Some(Hit::HorizontalScrollbar {
                    on_thumb: x >= thumb.start && x <= thumb.start + thumb.length,
                });
            }
        let x = f32::from(position.x) - self.body_left();
        let y = f32::from(position.y) - f32::from(self.bounds.origin.y);
        let header = self.metrics.header_height;
        if y < header {
            if x < 0.0 {
                return Some(Hit::Corner);
            }
            if let Some(col) = self
                .layout
                .resize_handle_at(x, self.scroll_x, (self.metrics.rem * 0.25).max(3.0))
            {
                return Some(Hit::ResizeHandle(col));
            }
            let col = self.layout.column_at(x, self.scroll_x)?;
            let (chart_top, chart_bottom) = self.chart_rows();
            if y >= chart_top && y < chart_bottom {
                return Some(Hit::HeaderChart(col, None));
            }
            return Some(Hit::Header(col));
        }
        let row_offset = (y - header) as f64 / self.metrics.row_height as f64;
        let row = (self.viewport.top + row_offset).floor();
        if row < 0.0 || row as u64 >= self.viewport.row_count {
            return None;
        }
        let row = row as u64;
        if x < 0.0 {
            return Some(Hit::RowHeader(row));
        }
        let col = self.layout.column_at(x, self.scroll_x)?;
        Some(Hit::Cell(CellPos::new(row, col)))
    }

    /// Vertical span of the summary chart within the header (relative to the grid top).
    pub fn chart_rows(&self) -> (f32, f32) {
        let m = &self.metrics;
        let top = m.padding * 0.75 + m.font_size * 1.35 + m.small_font_size * 1.3 + m.padding * 0.35;
        let bottom = m.header_height - m.small_font_size * 1.5 - m.padding * 0.6;
        (top, bottom.max(top))
    }

    /// Window bounds of a header cell.
    pub fn header_bounds(&self, col: usize) -> Bounds<Pixels> {
        let left = self.body_left() + self.layout.left(col, self.scroll_x);
        Bounds::new(
            point(px(left), self.bounds.origin.y),
            size(px(self.layout.width(col)), px(self.metrics.header_height)),
        )
    }

    pub fn rows_per_page(&self) -> u64 {
        (self.viewport.height_rows.floor() as u64).saturating_sub(1).max(1)
    }
}

pub struct GridState {
    focus_handle: FocusHandle,
    view: Option<View>,
    /// Bumped whenever the view changes; async results from older generations are dropped.
    generation: u64,
    columns: Vec<GridColumn>,
    /// Dataset column indices in display order (hidden columns are absent).
    display: Vec<usize>,
    pinned: usize,
    cache: PageCache,
    inflight: HashMap<BlockKey, Task<()>>,
    summaries: HashMap<usize, SummaryState>,
    summary_tasks: Vec<Task<()>>,
    stats_mode: StatsMode,
    show_summaries: bool,
    pub(crate) top: f64,
    /// Horizontal scroll in rem units.
    pub(crate) scroll_x: f32,
    selection: Option<Selection>,
    pub(crate) hover: Option<Hit>,
    pub(crate) hover_position: Option<Point<Pixels>>,
    pub(crate) drag: Option<Drag>,
    pub(crate) frame: Option<Frame>,
    context_target: Option<Hit>,
    /// Column widths were fitted to the first page of data.
    fitted: bool,
    /// Last visible range the fetcher was asked about, to avoid redundant work.
    requested: Option<(u64, Range<u64>, Range<usize>)>,
    loading_blocks_failed: HashSet<BlockKey>,
}

/// How columns are arranged, by name, so it can be saved and applied to the same
/// data later (even if columns were added or removed meanwhile).
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(default)]
pub struct ColumnArrangement {
    /// Shown columns in display order.
    pub order: Vec<String>,
    /// How many of `order` are pinned to the left.
    pub pinned: usize,
    pub hidden: Vec<String>,
    /// Widths set by hand or by fitting, in rem.
    pub widths: Vec<(String, f32)>,
}

impl EventEmitter<GridEvent> for GridState {}

impl Focusable for GridState {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl GridState {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle().tab_stop(true),
            view: None,
            generation: 0,
            columns: Vec::new(),
            display: Vec::new(),
            pinned: 0,
            cache: PageCache::default(),
            inflight: HashMap::new(),
            summaries: HashMap::new(),
            summary_tasks: Vec::new(),
            stats_mode: StatsMode::Auto,
            show_summaries: true,
            top: 0.0,
            scroll_x: 0.0,
            selection: None,
            hover: None,
            hover_position: None,
            drag: None,
            frame: None,
            context_target: None,
            fitted: false,
            requested: None,
            loading_blocks_failed: HashSet::new(),
        }
    }

    // ------------------------------------------------------------ data

    /// Show a view. Column widths, order and pins survive when the columns are the same
    /// (e.g. after filtering); scroll and selection reset unless `keep_position`.
    pub fn set_view(&mut self, view: Option<View>, keep_position: bool, cx: &mut Context<Self>) {
        let same_columns = match (&self.view, &view) {
            (Some(old), Some(new)) => old.columns() == new.columns(),
            _ => false,
        };
        let same_data = match (&self.view, &view) {
            // Sorting doesn't change distributions: keep summaries unless filters change.
            (Some(old), Some(new)) => {
                old.dataset.id == new.dataset.id
                    && old.spec.filters == new.spec.filters
                    && old.spec.search == new.spec.search
                    && old.spec.where_sql == new.spec.where_sql
            }
            _ => false,
        };
        self.generation += 1;
        let scattered = view.as_ref().is_some_and(|v| v.is_scattered());
        self.cache.set_block_rows(if scattered { SCATTERED_ROW_BLOCK } else { ROW_BLOCK });
        self.inflight.clear();
        self.requested = None;
        self.loading_blocks_failed.clear();
        if !same_data {
            self.summaries.clear();
            self.summary_tasks.clear();
        }
        if !same_columns {
            self.columns = view
                .as_ref()
                .map(|v| {
                    v.columns()
                        .iter()
                        .map(|info| GridColumn {
                            width: default_width(info),
                            info: info.clone(),
                            auto_width: true,
                        })
                        .collect()
                })
                .unwrap_or_default();
            self.display = (0..self.columns.len()).collect();
            self.pinned = 0;
            self.fitted = false;
            self.scroll_x = 0.0;
        }
        if !keep_position {
            self.top = 0.0;
            self.selection = None;
        }
        self.view = view;
        if let Some(selection) = self.selection {
            self.selection = selection.clamped(self.row_count(), self.display.len());
        }
        self.top = self.top.min(self.row_count() as f64);
        cx.emit(GridEvent::SelectionChanged);
        cx.notify();
    }

    pub fn view(&self) -> Option<&View> {
        self.view.as_ref()
    }

    pub fn row_count(&self) -> u64 {
        self.view.as_ref().map_or(0, |v| v.row_count)
    }

    pub fn columns(&self) -> &[GridColumn] {
        &self.columns
    }

    /// Dataset column indices in display order.
    pub fn display_columns(&self) -> &[usize] {
        &self.display
    }

    pub fn pinned_count(&self) -> usize {
        self.pinned
    }

    pub fn cell(&self, row: u64, column: usize) -> CellState<'_> {
        self.cache.cell(row, column)
    }

    pub fn summary(&self, column: usize) -> Option<&SummaryState> {
        self.summaries.get(&column)
    }

    pub fn show_summaries(&self) -> bool {
        self.show_summaries
    }

    pub fn set_show_summaries(&mut self, show: bool, cx: &mut Context<Self>) {
        self.show_summaries = show;
        cx.notify();
    }

    pub fn stats_mode(&self) -> StatsMode {
        self.stats_mode
    }

    /// Recompute all summaries (e.g. exactly instead of sampled).
    pub fn recompute_summaries(&mut self, mode: StatsMode, cx: &mut Context<Self>) {
        self.stats_mode = mode;
        self.summaries.clear();
        self.summary_tasks.clear();
        self.requested = None;
        cx.notify();
    }

    /// Retry blocks that failed to load.
    pub fn retry_failed(&mut self, cx: &mut Context<Self>) {
        self.cache.clear_failures();
        self.loading_blocks_failed.clear();
        self.requested = None;
        cx.notify();
    }

    pub fn is_loading(&self) -> bool {
        !self.inflight.is_empty()
    }

    pub fn is_summarizing(&self) -> bool {
        self.summaries.values().any(|s| matches!(s, SummaryState::Loading))
    }

    /// Make sure the blocks and summaries needed for the visible area are loaded or loading.
    /// Called during prepaint with the current visible ranges.
    pub(crate) fn ensure_visible(
        &mut self,
        rows: Range<u64>,
        display_cols: Range<usize>,
        pinned: usize,
        cx: &mut Context<Self>,
    ) {
        let Some(view) = self.view.clone() else {
            return;
        };
        let key = (self.generation, rows.clone(), display_cols.clone());
        if self.requested.as_ref() == Some(&key) {
            return;
        }
        self.requested = Some(key);

        // Dataset columns on screen: pinned plus visible scrolling ones.
        let mut dataset_cols: Vec<usize> = self.display[..pinned.min(self.display.len())].to_vec();
        dataset_cols.extend(
            self.display[display_cols.start.min(self.display.len())..display_cols.end.min(self.display.len())]
                .iter()
                .copied(),
        );

        // Blocks: visible rows plus a screen of prefetch in each direction.
        let height = rows.end - rows.start;
        let prefetch_rows = rows.start.saturating_sub(height)..(rows.end + height).min(view.row_count);
        let mut wanted: Vec<BlockKey> = Vec::new();
        let col_blocks: HashSet<usize> = dataset_cols.iter().map(|c| c / COL_BLOCK).collect();
        let mut col_blocks: Vec<usize> = col_blocks.into_iter().collect();
        col_blocks.sort_unstable();
        for block in col_blocks {
            let columns = block * COL_BLOCK..(block + 1) * COL_BLOCK;
            wanted.extend(blocks_for(prefetch_rows.clone(), columns, rows.start, self.cache.block_rows()));
        }
        // Visible blocks first.
        wanted.sort_by_key(|k| {
            let visible = k.rows().start < rows.end && k.rows().end > rows.start;
            (!visible, k.row_block.abs_diff(rows.start / k.block_rows), k.col_block)
        });
        let wanted_set: HashSet<BlockKey> = wanted.iter().copied().collect();
        // Cancel fetches for blocks that scrolled far away.
        self.inflight.retain(|key, _| wanted_set.contains(key));
        for key in wanted {
            if self.cache.contains(&key) {
                self.cache.touch(key);
                continue;
            }
            if self.inflight.contains_key(&key) || self.cache.has_failed(&key) {
                continue;
            }
            if self.inflight.len() >= MAX_INFLIGHT {
                break;
            }
            self.fetch_block(&view, key, cx);
        }

        if self.show_summaries {
            self.ensure_summaries(&view, &dataset_cols, cx);
        }
    }

    fn fetch_block(&mut self, view: &View, key: BlockKey, cx: &mut Context<Self>) {
        let job = view.fetch_page(PageRequest {
            rows: key.rows(),
            columns: key.columns(),
        });
        let generation = self.generation;
        let task = cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let result = job.await;
            let _ = this.update(cx, |this, cx| {
                if this.generation != generation {
                    return;
                }
                this.inflight.remove(&key);
                match result {
                    Ok(page) => {
                        this.cache.insert(key, Arc::new(page));
                        if !this.fitted && key.row_block == 0 {
                            this.fit_widths_to_data(key);
                        }
                    }
                    Err(error) if error.is_cancelled() => {}
                    Err(error) => {
                        this.cache.mark_failed(key);
                        if this.loading_blocks_failed.insert(key) && this.loading_blocks_failed.len() == 1 {
                            cx.emit(GridEvent::Error(error.to_string().into()));
                        }
                    }
                }
                // New data may unblock further fetches for the same viewport.
                this.requested = None;
                cx.notify();
            });
        });
        self.inflight.insert(key, task);
    }

    /// Load summaries for specific dataset columns (e.g. for an overview of all columns).
    pub fn request_summaries(&mut self, columns: &[usize], cx: &mut Context<Self>) {
        if let Some(view) = self.view.clone() {
            self.ensure_summaries(&view, columns, cx);
        }
    }

    fn ensure_summaries(&mut self, view: &View, columns: &[usize], cx: &mut Context<Self>) {
        let missing: Vec<usize> = columns
            .iter()
            .copied()
            .filter(|c| !self.summaries.contains_key(c))
            .collect();
        if missing.is_empty() {
            return;
        }
        for batch in missing.chunks(SUMMARY_BATCH) {
            for &c in batch {
                self.summaries.insert(c, SummaryState::Loading);
            }
            let job = summarize(view, batch.to_vec(), self.stats_mode);
            let generation = self.generation;
            let batch = batch.to_vec();
            let task = cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                let result = job.await;
                let _ = this.update(cx, |this, cx| {
                    if this.generation != generation {
                        return;
                    }
                    match result {
                        Ok(summaries) => {
                            for (column, summary) in batch.iter().zip(summaries) {
                                this.summaries.insert(*column, SummaryState::Ready(Arc::new(summary)));
                            }
                        }
                        Err(error) if error.is_cancelled() => {
                            for column in &batch {
                                this.summaries.remove(column);
                            }
                        }
                        Err(error) => {
                            let message: SharedString = error.to_string().into();
                            for column in &batch {
                                this.summaries.insert(*column, SummaryState::Failed(message.clone()));
                            }
                        }
                    }
                    cx.notify();
                });
            });
            self.summary_tasks.push(task);
        }
        // Keep the task list from growing without bound.
        if self.summary_tasks.len() > 256 {
            self.summary_tasks.drain(..128);
        }
    }

    /// Size auto-width columns to the text in the first block of rows.
    fn fit_widths_to_data(&mut self, key: BlockKey) {
        let char_rem = self
            .frame
            .as_ref()
            .map(|f| f.metrics.char_width / f.metrics.rem)
            .unwrap_or(0.5);
        let rows = key.rows();
        for column in key.columns() {
            let Some(grid_column) = self.columns.get(column) else {
                continue;
            };
            if !grid_column.auto_width {
                continue;
            }
            let mut longest = 0usize;
            for row in rows.clone() {
                if let CellState::Loaded(Some(text)) = self.cache.cell(row, column) {
                    longest = longest.max(text.chars().count());
                }
            }
            let width = fitted_width(&grid_column.info, longest, char_rem);
            self.columns[column].width = width;
        }
        if key.col_block == 0 {
            self.fitted = true;
        }
    }

    // ------------------------------------------------------------ columns

    /// The current column arrangement.
    pub fn column_arrangement(&self) -> ColumnArrangement {
        let name = |ix: &usize| self.columns[*ix].info.name.clone();
        ColumnArrangement {
            order: self.display.iter().map(name).collect(),
            pinned: self.pinned,
            hidden: (0..self.columns.len()).filter(|c| !self.display.contains(c)).map(|c| name(&c)).collect(),
            widths: self
                .columns
                .iter()
                .filter(|c| !c.auto_width)
                .map(|c| (c.info.name.clone(), c.width))
                .collect(),
        }
    }

    /// Arrange columns as saved. Unknown names are ignored; columns the layout doesn't
    /// mention (new since it was saved) are shown at the end.
    pub fn apply_column_arrangement(&mut self, layout: &ColumnArrangement, cx: &mut Context<Self>) {
        let index = |name: &str| self.columns.iter().position(|c| c.info.name == name);
        let mut display: Vec<usize> = Vec::with_capacity(self.columns.len());
        let mut pinned = 0;
        for (ix, name) in layout.order.iter().enumerate() {
            if let Some(column) = index(name).filter(|c| !display.contains(c)) {
                display.push(column);
                if ix < layout.pinned {
                    pinned += 1;
                }
            }
        }
        let hidden: Vec<usize> = layout.hidden.iter().filter_map(|n| index(n)).collect();
        for column in 0..self.columns.len() {
            if !display.contains(&column) && !hidden.contains(&column) {
                display.push(column);
            }
        }
        for (name, width) in &layout.widths {
            if let Some(c) = self.columns.iter_mut().find(|c| c.info.name == *name) {
                c.width = width.clamp(2.5, 80.0);
                c.auto_width = false;
            }
        }
        self.display = display;
        self.pinned = pinned;
        self.selection = self.selection.and_then(|s| s.clamped(self.row_count(), self.display.len()));
        self.requested = None;
        cx.notify();
    }

    /// The first row on screen.
    pub fn top_row(&self) -> u64 {
        self.top.floor().max(0.0) as u64
    }

    pub fn column_width(&self, column: usize) -> f32 {
        self.columns.get(column).map_or(8.0, |c| c.width)
    }

    pub fn set_column_width(&mut self, column: usize, width_rem: f32, cx: &mut Context<Self>) {
        if let Some(c) = self.columns.get_mut(column) {
            c.width = width_rem.clamp(2.5, 80.0);
            c.auto_width = false;
            cx.notify();
        }
    }

    /// Fit a column to its loaded contents.
    pub fn autofit_column(&mut self, column: usize, cx: &mut Context<Self>) {
        let char_rem = self
            .frame
            .as_ref()
            .map(|f| f.metrics.char_width / f.metrics.rem)
            .unwrap_or(0.5);
        let mut longest = 0usize;
        if let Some(frame) = &self.frame {
            for row in frame.viewport.visible_rows() {
                if let CellState::Loaded(Some(text)) = self.cache.cell(row, column) {
                    longest = longest.max(text.chars().count());
                }
            }
        }
        if let Some(c) = self.columns.get_mut(column) {
            c.width = fitted_width(&c.info, longest, char_rem).max(3.0);
            c.auto_width = false;
            cx.notify();
        }
    }

    pub fn is_hidden(&self, column: usize) -> bool {
        !self.display.contains(&column)
    }

    pub fn set_hidden(&mut self, column: usize, hidden: bool, cx: &mut Context<Self>) {
        let position = self.display.iter().position(|&c| c == column);
        match (hidden, position) {
            (true, Some(ix)) => {
                self.display.remove(ix);
                if ix < self.pinned {
                    self.pinned -= 1;
                }
            }
            (false, None) if column < self.columns.len() => {
                // Reinsert at its natural position among the unpinned columns.
                let at = self
                    .display
                    .iter()
                    .enumerate()
                    .skip(self.pinned)
                    .find(|(_, c)| **c > column)
                    .map(|(ix, _)| ix)
                    .unwrap_or(self.display.len());
                self.display.insert(at, column);
            }
            _ => return,
        }
        self.selection = self
            .selection
            .and_then(|s| s.clamped(self.row_count(), self.display.len()));
        self.requested = None;
        cx.notify();
    }

    pub fn show_all_columns(&mut self, cx: &mut Context<Self>) {
        let pinned: Vec<usize> = self.display[..self.pinned].to_vec();
        let mut rest: Vec<usize> = (0..self.columns.len()).filter(|c| !pinned.contains(c)).collect();
        rest.sort_unstable();
        self.display = pinned;
        self.display.extend(rest);
        self.requested = None;
        cx.notify();
    }

    pub fn is_pinned(&self, column: usize) -> bool {
        self.display[..self.pinned].contains(&column)
    }

    /// Pin a column to the left edge (after already pinned ones), or unpin it.
    pub fn set_pinned(&mut self, column: usize, pinned: bool, cx: &mut Context<Self>) {
        let Some(ix) = self.display.iter().position(|&c| c == column) else {
            return;
        };
        if pinned && ix >= self.pinned {
            self.display.remove(ix);
            self.display.insert(self.pinned, column);
            self.pinned += 1;
        } else if !pinned && ix < self.pinned {
            self.display.remove(ix);
            self.pinned -= 1;
            let at = self
                .display
                .iter()
                .enumerate()
                .skip(self.pinned)
                .find(|(_, c)| **c > column)
                .map(|(ix, _)| ix)
                .unwrap_or(self.display.len());
            self.display.insert(at, column);
        } else {
            return;
        }
        self.selection = None;
        self.requested = None;
        cx.notify();
    }

    /// Move a displayed column to another display position within its region.
    pub fn move_column(&mut self, from: usize, to: usize, cx: &mut Context<Self>) {
        if from >= self.display.len() || to >= self.display.len() || from == to {
            return;
        }
        let region = |ix: usize| ix < self.pinned;
        if region(from) != region(to) {
            return;
        }
        let column = self.display.remove(from);
        self.display.insert(to, column);
        self.requested = None;
        cx.notify();
    }

    // ------------------------------------------------------------ scrolling

    pub fn scroll_to_row(&mut self, row: u64, cx: &mut Context<Self>) {
        if let Some(frame) = &self.frame {
            let viewport = RowViewport { top: self.top, ..frame.viewport };
            self.top = viewport.clamp_top(row as f64);
        } else {
            self.top = row as f64;
        }
        cx.notify();
    }

    /// Scroll so a dataset column is visible, and select its header cell.
    pub fn reveal_column(&mut self, column: usize, cx: &mut Context<Self>) {
        let Some(display_ix) = self.display.iter().position(|&c| c == column) else {
            return;
        };
        if let Some(frame) = &self.frame {
            let rem = frame.metrics.rem;
            let target = frame
                .layout
                .scroll_to_reveal(display_ix, self.scroll_x * rem, frame.body_width());
            self.scroll_x = target / rem;
        }
        let row = self
            .selection
            .map(|s| s.head.row)
            .unwrap_or(self.top.ceil() as u64)
            .min(self.row_count().saturating_sub(1));
        if self.row_count() > 0 {
            self.set_selection(Some(Selection::cell(CellPos::new(row, display_ix))), cx);
        }
        cx.notify();
    }

    pub(crate) fn scroll_by(&mut self, rows: f64, x_px: f32, cx: &mut Context<Self>) -> bool {
        let Some(frame) = &self.frame else {
            return false;
        };
        let viewport = RowViewport { top: self.top, ..frame.viewport };
        let rem = frame.metrics.rem;
        let new_top = viewport.clamp_top(self.top + rows);
        let max_x = frame.layout.max_scroll_x(frame.body_width());
        let new_x = ((self.scroll_x * rem + x_px).clamp(0.0, max_x)) / rem;
        let changed = (new_top - self.top).abs() > f64::EPSILON || (new_x - self.scroll_x).abs() > f32::EPSILON;
        if changed {
            self.top = new_top;
            self.scroll_x = new_x;
            cx.notify();
        }
        changed
    }

    fn reveal(&mut self, pos: CellPos) {
        let Some(frame) = &self.frame else {
            return;
        };
        let viewport = RowViewport { top: self.top, ..frame.viewport };
        self.top = viewport.reveal(pos.row);
        let rem = frame.metrics.rem;
        let x = frame
            .layout
            .scroll_to_reveal(pos.col, self.scroll_x * rem, frame.body_width());
        self.scroll_x = x / rem;
    }

    // ------------------------------------------------------------ selection

    pub fn selection(&self) -> Option<Selection> {
        self.selection
    }

    pub fn set_selection(&mut self, selection: Option<Selection>, cx: &mut Context<Self>) {
        if self.selection != selection {
            self.selection = selection;
            cx.emit(GridEvent::SelectionChanged);
            cx.notify();
        }
    }

    /// Active cell as (row, dataset column).
    pub fn active_cell(&self) -> Option<(u64, usize)> {
        let s = self.selection?;
        let column = *self.display.get(s.head.col)?;
        (s.head.row < self.row_count()).then_some((s.head.row, column))
    }

    /// Selected rows and dataset columns (in display order).
    pub fn selected_ranges(&self) -> Option<(Range<u64>, Vec<usize>)> {
        let s = self.selection?;
        let rows = s.rows(self.row_count());
        let cols: Vec<usize> = s
            .columns(self.display.len())
            .filter_map(|ix| self.display.get(ix).copied())
            .collect();
        if rows.is_empty() || cols.is_empty() {
            None
        } else {
            Some((rows, cols))
        }
    }

    pub(crate) fn move_selection(&mut self, movement: Movement, extend: bool, cx: &mut Context<Self>) {
        let rows = self.row_count();
        let cols = self.display.len();
        if rows == 0 || cols == 0 {
            return;
        }
        let page = self.frame.as_ref().map_or(20, |f| f.rows_per_page());
        let current = self.selection.unwrap_or_else(|| {
            let row = (self.top.ceil() as u64).min(rows - 1);
            Selection::cell(CellPos::new(row, 0))
        });
        let head = if self.selection.is_some() {
            move_head(current.head, movement, rows, cols, page)
        } else {
            current.head
        };
        let selection = if extend {
            Selection {
                anchor: current.anchor,
                head,
                kind: if current.kind == SelectionKind::Cells { SelectionKind::Cells } else { current.kind },
            }
        } else {
            Selection::cell(head)
        };
        self.reveal(head);
        self.set_selection(Some(selection), cx);
        cx.notify();
    }

    pub fn select_all(&mut self, cx: &mut Context<Self>) {
        let rows = self.row_count();
        let cols = self.display.len();
        if rows == 0 || cols == 0 {
            return;
        }
        self.set_selection(
            Some(Selection {
                anchor: CellPos::new(0, 0),
                head: CellPos::new(rows - 1, cols - 1),
                kind: SelectionKind::Cells,
            }),
            cx,
        );
    }

    pub fn clear_selection(&mut self, cx: &mut Context<Self>) {
        self.set_selection(None, cx);
    }

    pub fn context_target(&self) -> Option<Hit> {
        self.context_target
    }

    /// Resolve what a right-click at `position` targets, selecting the cell under it
    /// unless it's already part of the selection.
    pub(crate) fn prepare_context_menu(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) -> Option<Hit> {
        let hit = self.frame.as_ref()?.hit(position)?;
        self.context_target = Some(hit);
        match hit {
            Hit::Cell(pos) => {
                let inside = self
                    .selection
                    .is_some_and(|s| s.contains(pos.row, pos.col, self.row_count(), self.display.len()));
                if !inside {
                    self.set_selection(Some(Selection::cell(pos)), cx);
                }
            }
            Hit::RowHeader(row) => {
                let inside = self.selection.is_some_and(|s| {
                    s.kind == SelectionKind::Rows && s.rows(self.row_count()).contains(&row)
                });
                if !inside {
                    self.set_selection(
                        Some(Selection {
                            anchor: CellPos::new(row, 0),
                            head: CellPos::new(row, 0),
                            kind: SelectionKind::Rows,
                        }),
                        cx,
                    );
                }
            }
            _ => {}
        }
        Some(hit)
    }

    // ------------------------------------------------------------ copy

    /// Copy the selection to the clipboard. Values are fetched in full (not the
    /// truncated display text), up to [`MAX_COPY_ROWS`] rows.
    pub fn copy_selection(&mut self, format: CopyFormat, with_headers: bool, cx: &mut Context<Self>) {
        let (Some(view), Some((rows, cols))) = (self.view.clone(), self.selected_ranges()) else {
            return;
        };
        let total_rows = (rows.end - rows.start) as usize;
        let truncated = total_rows > MAX_COPY_ROWS;
        let headers: Vec<String> = cols.iter().map(|&c| self.columns[c].info.name.clone()).collect();
        if format == CopyFormat::Json {
            let job = view.fetch_json(rows, cols, MAX_COPY_ROWS);
            cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                let result = job.await;
                let _ = this.update(cx, |_, cx| match result {
                    Ok(lines) => {
                        let text = if lines.len() == 1 {
                            lines[0].clone()
                        } else {
                            format!("[\n  {}\n]", lines.join(",\n  "))
                        };
                        cx.write_to_clipboard(ClipboardItem::new_string(text));
                        cx.emit(GridEvent::Copied { rows: lines.len(), truncated });
                    }
                    Err(error) => cx.emit(GridEvent::Error(error.to_string().into())),
                });
            })
            .detach();
            return;
        }
        let job = view.fetch_values(rows, cols, MAX_COPY_ROWS);
        cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let result = job.await;
            let _ = this.update(cx, |_, cx| match result {
                Ok(values) => {
                    let headers = with_headers.then_some(headers.as_slice());
                    match format_cells(headers, &values, format) {
                        Ok(text) => {
                            cx.write_to_clipboard(ClipboardItem::new_string(text));
                            cx.emit(GridEvent::Copied { rows: values.len(), truncated });
                        }
                        Err(error) => cx.emit(GridEvent::Error(error.to_string().into())),
                    }
                }
                Err(error) => cx.emit(GridEvent::Error(error.to_string().into())),
            });
        })
        .detach();
    }

    // ------------------------------------------------------------ pointer

    pub(crate) fn on_mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let Some(frame) = self.frame.clone() else {
            return;
        };
        let Some(hit) = frame.hit(event.position) else {
            return;
        };
        if event.button == MouseButton::Right {
            self.context_target = Some(hit);
            if let Hit::Cell(pos) = hit {
                let inside = self
                    .selection
                    .is_some_and(|s| s.contains(pos.row, pos.col, self.row_count(), self.display.len()));
                if !inside {
                    self.set_selection(Some(Selection::cell(pos)), cx);
                }
            }
            return;
        }
        if event.button != MouseButton::Left {
            return;
        }
        window.focus(&self.focus_handle, cx);
        let shift = event.modifiers.shift;
        match hit {
            Hit::VerticalScrollbar { on_thumb } => {
                let (track, thumb) = frame.vertical_track.unwrap();
                let y = f32::from(event.position.y - track.origin.y);
                if on_thumb {
                    self.drag = Some(Drag::VerticalThumb { grab: y - thumb.start });
                } else {
                    let page = frame.viewport.height_rows;
                    let direction = if y < thumb.start { -1.0 } else { 1.0 };
                    self.scroll_by(direction * page, 0.0, cx);
                }
            }
            Hit::HorizontalScrollbar { on_thumb } => {
                let (track, thumb) = frame.horizontal_track.unwrap();
                let x = f32::from(event.position.x - track.origin.x);
                if on_thumb {
                    self.drag = Some(Drag::HorizontalThumb { grab: x - thumb.start });
                } else {
                    let direction = if x < thumb.start { -1.0 } else { 1.0 };
                    self.scroll_by(0.0, direction * frame.body_width() * 0.9, cx);
                }
            }
            Hit::ResizeHandle(col) => {
                let column = self.display[col];
                if event.click_count >= 2 {
                    self.autofit_column(column, cx);
                } else {
                    self.drag = Some(Drag::Resize {
                        col,
                        start_x: f32::from(event.position.x),
                        start_width: self.column_width(column),
                    });
                }
            }
            Hit::Header(col) | Hit::HeaderChart(col, _) => {
                if event.modifiers.platform || shift {
                    let anchor = if shift {
                        self.selection
                            .filter(|s| s.kind == SelectionKind::Columns)
                            .map(|s| s.anchor)
                            .unwrap_or(CellPos::new(0, col))
                    } else {
                        CellPos::new(0, col)
                    };
                    self.set_selection(
                        Some(Selection {
                            anchor,
                            head: CellPos::new(0, col),
                            kind: SelectionKind::Columns,
                        }),
                        cx,
                    );
                    self.drag = Some(Drag::SelectColumns);
                } else {
                    let bar = match hit {
                        Hit::HeaderChart(..) => self.chart_item_at(col, event.position, &frame),
                        _ => None,
                    };
                    self.drag = Some(Drag::HeaderPress { col, start: event.position, bar });
                }
            }
            Hit::RowHeader(row) => {
                let anchor = if shift {
                    self.selection.map(|s| s.anchor).unwrap_or(CellPos::new(row, 0))
                } else {
                    CellPos::new(row, 0)
                };
                self.set_selection(
                    Some(Selection {
                        anchor,
                        head: CellPos::new(row, 0),
                        kind: SelectionKind::Rows,
                    }),
                    cx,
                );
                self.drag = Some(Drag::SelectRows);
            }
            Hit::Cell(pos) => {
                if event.click_count >= 2 {
                    self.set_selection(Some(Selection::cell(pos)), cx);
                    if let Some(&column) = self.display.get(pos.col) {
                        cx.emit(GridEvent::InspectCell { row: pos.row, column });
                    }
                    return;
                }
                let selection = match (shift, self.selection) {
                    (true, Some(current)) => Selection {
                        anchor: current.anchor,
                        head: pos,
                        kind: SelectionKind::Cells,
                    },
                    _ => Selection::cell(pos),
                };
                self.set_selection(Some(selection), cx);
                self.drag = Some(Drag::SelectCells);
            }
            Hit::Corner => self.select_all(cx),
        }
    }

    pub(crate) fn on_mouse_move(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        let Some(frame) = self.frame.clone() else {
            return;
        };
        if let Some(drag) = self.drag {
            if event.pressed_button != Some(MouseButton::Left) {
                self.drag = None;
            } else {
                self.continue_drag(drag, event.position, &frame, cx);
                return;
            }
        }
        let mut hit = frame.hit(event.position);
        if let Some(Hit::HeaderChart(col, _)) = hit {
            hit = Some(Hit::HeaderChart(col, self.chart_item_at(col, event.position, &frame)));
        }
        let position_changed = matches!(hit, Some(Hit::HeaderChart(..))) && self.hover_position != Some(event.position);
        if hit != self.hover || position_changed {
            self.hover = hit;
            self.hover_position = Some(event.position);
            cx.notify();
        }
    }

    pub(crate) fn on_mouse_up(&mut self, event: &MouseUpEvent, cx: &mut Context<Self>) {
        let Some(drag) = self.drag.take() else {
            return;
        };
        if let Drag::HeaderPress { col, start, bar } = drag {
            let moved = (f32::from(event.position.x - start.x)).abs() + (f32::from(event.position.y - start.y)).abs();
            if moved < 4.0
                && let (Some(frame), Some(&column)) = (&self.frame, self.display.get(col)) {
                    let anchor = frame.header_bounds(col);
                    cx.emit(match bar {
                        Some(bar) => GridEvent::ChartBarClicked { column, bar, anchor },
                        None => GridEvent::HeaderClicked { column, anchor },
                    });
                }
        }
        cx.notify();
    }

    pub(crate) fn on_hover_end(&mut self, cx: &mut Context<Self>) {
        if self.hover.is_some() {
            self.hover = None;
            self.hover_position = None;
            cx.notify();
        }
    }

    pub(crate) fn on_scroll(&mut self, event: &ScrollWheelEvent, cx: &mut Context<Self>) -> bool {
        let Some(frame) = &self.frame else {
            return false;
        };
        let row_height = frame.metrics.row_height;
        let delta = event.delta.pixel_delta(px(row_height));
        let (mut dx, mut dy) = (f32::from(delta.x), f32::from(delta.y));
        if event.modifiers.shift && dx == 0.0 {
            dx = dy;
            dy = 0.0;
        }
        self.scroll_by(-(dy as f64) / row_height as f64, -dx, cx)
    }

    fn continue_drag(&mut self, drag: Drag, position: Point<Pixels>, frame: &Frame, cx: &mut Context<Self>) {
        match drag {
            Drag::Resize { col, start_x, start_width } => {
                let column = self.display[col];
                let delta_rem = (f32::from(position.x) - start_x) / frame.metrics.rem;
                self.set_column_width(column, start_width + delta_rem, cx);
            }
            Drag::VerticalThumb { grab } => {
                if let Some((track, thumb)) = frame.vertical_track {
                    let start = f32::from(position.y - track.origin.y) - grab;
                    let content = frame.viewport.row_count as f64;
                    let top = offset_for_thumb(content, frame.viewport.height_rows, start, thumb.length, f32::from(track.size.height));
                    let viewport = RowViewport { top, ..frame.viewport };
                    self.top = viewport.clamp_top(top);
                    cx.notify();
                }
            }
            Drag::HorizontalThumb { grab } => {
                if let Some((track, thumb)) = frame.horizontal_track {
                    let start = f32::from(position.x - track.origin.x) - grab;
                    let visible = (frame.body_width() - frame.layout.pinned_width()).max(0.0) as f64;
                    let content = frame.layout.scrolling_width() as f64;
                    let x = offset_for_thumb(content, visible, start, thumb.length, f32::from(track.size.width));
                    self.scroll_x = x as f32 / frame.metrics.rem;
                    cx.notify();
                }
            }
            Drag::SelectCells | Drag::SelectRows | Drag::SelectColumns => {
                self.autoscroll_toward(position, frame, cx);
                let Some(frame) = self.frame.clone() else { return };
                let clamped = clamp_into_body(position, &frame);
                if let Some(mut selection) = self.selection {
                    match (drag, frame.hit(clamped)) {
                        (Drag::SelectCells, Some(Hit::Cell(pos))) => selection.head = pos,
                        (Drag::SelectRows, Some(Hit::Cell(pos))) => selection.head.row = pos.row,
                        (Drag::SelectRows, Some(Hit::RowHeader(row))) => selection.head.row = row,
                        (Drag::SelectColumns, Some(Hit::Header(col) | Hit::HeaderChart(col, _))) => {
                            selection.head.col = col
                        }
                        (Drag::SelectColumns, Some(Hit::Cell(pos))) => selection.head.col = pos.col,
                        _ => {}
                    }
                    self.set_selection(Some(selection), cx);
                }
            }
            Drag::HeaderPress { col, start, .. } => {
                // Dragging a header horizontally reorders it.
                let dx = f32::from(position.x - start.x);
                if dx.abs() > frame.metrics.rem {
                    let x = f32::from(position.x) - frame.body_left();
                    if let Some(target) = frame.layout.column_at(x, frame.scroll_x)
                        && target != col {
                            self.move_column(col, target, cx);
                            self.drag = Some(Drag::HeaderPress { col: target, start: position, bar: None });
                        }
                }
            }
        }
    }

    fn autoscroll_toward(&mut self, position: Point<Pixels>, frame: &Frame, cx: &mut Context<Self>) {
        let y = f32::from(position.y);
        let x = f32::from(position.x);
        let mut rows = 0.0;
        let mut dx = 0.0;
        if y < frame.body_top() {
            rows = -1.0;
        } else if y > frame.body_top() + frame.body_height() {
            rows = 1.0;
        }
        if x < frame.body_left() + frame.layout.pinned_width() && frame.scroll_x > 0.0 && self.drag.is_some_and(|d| !matches!(d, Drag::SelectRows)) {
            dx = -frame.metrics.rem * 2.0;
        } else if x > frame.body_left() + frame.body_width() {
            dx = frame.metrics.rem * 2.0;
        }
        if rows != 0.0 || dx != 0.0 {
            self.scroll_by(rows, dx, cx);
        }
    }

    /// Histogram bin or top-value bar under a position in a header chart.
    fn chart_item_at(&self, col: usize, position: Point<Pixels>, frame: &Frame) -> Option<usize> {
        let column = *self.display.get(col)?;
        let SummaryState::Ready(summary) = self.summaries.get(&column)? else {
            return None;
        };
        let bounds = frame.header_bounds(col);
        let inset = frame.metrics.padding;
        let left = f32::from(bounds.origin.x) + inset;
        let width = f32::from(bounds.size.width) - inset * 2.0;
        if width <= 0.0 {
            return None;
        }
        let fraction = ((f32::from(position.x) - left) / width).clamp(0.0, 0.9999);
        if !summary.histogram.is_empty() {
            return Some((fraction * summary.histogram.len() as f32) as usize);
        }
        crate::chart::top_value_at(summary, fraction)
    }

    /// Bounds of a displayed column's header chart relative to the grid element
    /// (`data-grid-root`), as last painted.
    pub fn chart_bounds(&self, display_col: usize) -> Option<Bounds<Pixels>> {
        let frame = self.frame.as_ref()?;
        let header = frame.header_bounds(display_col);
        let (top, bottom) = frame.chart_rows();
        let inset = frame.metrics.padding;
        let origin = header.origin - frame.bounds.origin;
        Some(Bounds::new(
            point(origin.x + px(inset), origin.y + px(top)),
            size(header.size.width - px(inset * 2.0), px(bottom - top)),
        ))
    }

    pub(crate) fn set_frame(&mut self, frame: Frame) {
        self.frame = Some(frame);
    }

}

fn clamp_into_body(position: Point<Pixels>, frame: &Frame) -> Point<Pixels> {
    let x = f32::from(position.x).clamp(frame.body_left(), frame.body_left() + frame.body_width() - 1.0);
    let y = f32::from(position.y).clamp(frame.body_top(), frame.body_top() + frame.body_height() - 1.0);
    point(px(x), px(y))
}

/// Initial column width in rem, from its type and name.
pub fn default_width(info: &ColumnInfo) -> f32 {
    let chars = match info.kind {
        ColumnKind::Boolean => 6,
        ColumnKind::Integer => 10,
        ColumnKind::Float | ColumnKind::Decimal => 12,
        ColumnKind::Date => 10,
        ColumnKind::Timestamp => 19,
        ColumnKind::Time => 8,
        ColumnKind::Interval => 12,
        ColumnKind::Uuid => 36,
        ColumnKind::Binary => 14,
        ColumnKind::String => 16,
        ColumnKind::List | ColumnKind::Struct | ColumnKind::Map | ColumnKind::Other => 22,
    };
    fitted_width(info, chars, 0.5)
}

/// Width in rem for `chars` characters of data, never narrower than the header needs.
fn fitted_width(info: &ColumnInfo, chars: usize, char_rem: f32) -> f32 {
    let data = chars.min(48) as f32 * char_rem + 1.25;
    // The header name uses the UI font (a bit narrower than mono) plus a sort arrow.
    let name = info.name.chars().count().min(32) as f32 * char_rem * 0.9 + 2.25;
    let type_label = info.type_label().chars().count().min(24) as f32 * char_rem * 0.8 + 1.25;
    data.max(name).max(type_label).clamp(7.5, 30.0)
}

#[cfg(test)]
mod tests {
    use super::default_width;
    use parquetry_engine::ColumnInfo;

    #[test]
    fn widths() {
        let w = default_width(&ColumnInfo::new("id", "BIGINT"));
        assert!((6.5..=30.0).contains(&w));
        let long = default_width(&ColumnInfo::new("a_really_long_column_name_that_goes_on", "VARCHAR"));
        assert!(long > w);
        assert!(long <= 30.0);
    }
}
