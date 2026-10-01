//! The painted grid.
//!
//! One custom element draws the whole table: header, row gutter, cells and
//! scrollbars. Only visible cells are shaped and painted, so the cost of a frame
//! depends on the window size, not on the number of rows or columns. Text is
//! shaped through GPUI's line cache, which reuses the previous frame's layouts
//! while scrolling.

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::*;
use parquetry_engine::ColumnKind;

use crate::cache::CellState;
use crate::chart::{chart_colors, chart_placeholder, footer_labels, kind_badge, kind_color, null_color, null_label, paint_chart, thousands};
use crate::layout::{ColumnLayout, RowViewport, fitting_chars, thumb, truncate_chars};
use crate::selection::SelectionKind;
use crate::state::{Drag, Frame, GridState, Hit, Metrics, SummaryState};

pub struct GridElement {
    state: Entity<GridState>,
}

impl GridElement {
    pub fn new(state: Entity<GridState>) -> Self {
        Self { state }
    }
}

impl IntoElement for GridElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

pub struct GridPrepaint {
    hitbox: Hitbox,
    frame: Frame,
    visible_rows: std::ops::Range<u64>,
    visible_scrolling: std::ops::Range<usize>,
    snapshot: Snapshot,
}

/// Everything painting needs from the state, copied (cheaply: `Arc`s) so that
/// painting can borrow the app mutably.
struct Snapshot {
    /// Displayed columns on screen, by display index.
    columns: std::collections::HashMap<usize, ColumnSnap>,
    selection: Option<crate::selection::Selection>,
    hover: Option<Hit>,
    dragging_resize: bool,
    dragging_vertical: bool,
    dragging_horizontal: bool,
    row_count: u64,
    display_len: usize,
    sort: Vec<parquetry_engine::SortKey>,
    focused: bool,
    show_summaries: bool,
    empty_message: bool,
}

struct ColumnSnap {
    column: usize,
    info: parquetry_engine::ColumnInfo,
    summary: Option<SummaryState>,
    /// One entry per visible row, in order.
    cells: Vec<CellSnap>,
}

enum CellSnap {
    Text(std::sync::Arc<str>),
    Null,
    Loading,
    Failed,
}

struct Fonts {
    mono: Font,
    mono_italic: Font,
    mono_bold: Font,
    ui: Font,
    ui_bold: Font,
}

impl Element for GridElement {
    type RequestLayoutState = ();
    type PrepaintState = GridPrepaint;

    fn id(&self) -> Option<ElementId> {
        Some("data-grid".into())
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let rem = f32::from(window.rem_size());
        let theme = cx.theme();
        let mono = font(theme.mono_font_family.clone());
        let show_summaries = self.state.read(cx).show_summaries();
        let probe = Metrics::new(rem, 0.0, show_summaries);
        let font_id = window.text_system().resolve_font(&mono);
        let char_width = window
            .text_system()
            .advance(font_id, px(probe.font_size), '0')
            .map(|s| f32::from(s.width))
            .unwrap_or(probe.font_size * 0.6);
        let metrics = Metrics::new(rem, char_width, show_summaries);

        let state = self.state.read(cx);
        let widths: Vec<f32> = state
            .display_columns()
            .iter()
            .map(|&c| (state.column_width(c) * rem).round())
            .collect();
        let layout = ColumnLayout::new(widths, state.pinned_count());
        let row_count = state.row_count();
        let digits = thousands(row_count.saturating_sub(1)).len().max(3);
        let gutter = ((digits as f32 + 0.5) * char_width + metrics.padding * 2.0).ceil();

        let body_width = (f32::from(bounds.size.width) - gutter).max(0.0);
        let body_height = (f32::from(bounds.size.height) - metrics.header_height).max(0.0);
        let height_rows = body_height as f64 / metrics.row_height as f64;
        let mut viewport = RowViewport {
            top: state.top,
            height_rows,
            row_count,
        };
        viewport.top = viewport.clamp_top(viewport.top);
        let max_x = layout.max_scroll_x(body_width);
        let scroll_x = (state.scroll_x * rem).clamp(0.0, max_x);

        let bar = metrics.scrollbar;
        let vertical_track = {
            let track = Bounds::new(
                point(bounds.right() - px(bar), bounds.origin.y + px(metrics.header_height)),
                size(px(bar), px(body_height)),
            );
            thumb(
                row_count as f64,
                height_rows,
                viewport.top,
                body_height,
                metrics.rem * 1.5,
            )
            .map(|t| (track, t))
        };
        let scroll_visible = (body_width - layout.pinned_width()).max(0.0);
        let horizontal_track = {
            let left = f32::from(bounds.origin.x) + gutter + layout.pinned_width();
            let track = Bounds::new(
                point(px(left), bounds.bottom() - px(bar)),
                size(px(scroll_visible - if vertical_track.is_some() { bar } else { 0.0 }), px(bar)),
            );
            thumb(
                layout.scrolling_width() as f64,
                scroll_visible as f64,
                scroll_x as f64,
                f32::from(track.size.width),
                metrics.rem * 1.5,
            )
            .map(|t| (track, t))
        };

        let visible_rows = viewport.visible_rows();
        let visible_scrolling = layout.visible_scrolling(scroll_x, scroll_visible);
        let frame = Frame {
            bounds,
            metrics,
            gutter,
            layout,
            scroll_x,
            viewport,
            vertical_track,
            horizontal_track,
        };
        let pinned = frame.layout.pinned();
        let clamped_top = viewport.top;
        let clamped_x = scroll_x / rem;
        let rows = visible_rows.clone();
        let cols = visible_scrolling.clone();
        let stored = frame.clone();
        self.state.update(cx, |state, cx| {
            state.top = clamped_top;
            state.scroll_x = clamped_x;
            state.set_frame(stored);
            state.ensure_visible(rows, cols, pinned, cx);
        });

        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        let snapshot = {
            let state = self.state.read(cx);
            let display = state.display_columns();
            let mut columns = std::collections::HashMap::new();
            let on_screen = (0..pinned).chain(visible_scrolling.clone());
            for display_ix in on_screen {
                let Some(&column) = display.get(display_ix) else { continue };
                let cells = visible_rows
                    .clone()
                    .map(|row| match state.cell(row, column) {
                        CellState::Loaded(Some(text)) => CellSnap::Text(text.clone()),
                        CellState::Loaded(None) => CellSnap::Null,
                        CellState::Loading => CellSnap::Loading,
                        CellState::Failed => CellSnap::Failed,
                    })
                    .collect();
                columns.insert(
                    display_ix,
                    ColumnSnap {
                        column,
                        info: state.columns()[column].info.clone(),
                        summary: state.summary(column).cloned(),
                        cells,
                    },
                );
            }
            Snapshot {
                columns,
                selection: state.selection(),
                hover: state.hover,
                dragging_resize: matches!(state.drag, Some(Drag::Resize { .. })),
                dragging_vertical: matches!(state.drag, Some(Drag::VerticalThumb { .. })),
                dragging_horizontal: matches!(state.drag, Some(Drag::HorizontalThumb { .. })),
                row_count: state.row_count(),
                display_len: display.len(),
                sort: state.view().map(|v| v.spec.sort.clone()).unwrap_or_default(),
                focused: state.focus_handle(cx).is_focused(window),
                show_summaries: state.show_summaries(),
                empty_message: state.row_count() == 0 && !state.is_loading() && state.view().is_some(),
            }
        };
        GridPrepaint {
            hitbox,
            frame,
            visible_rows,
            visible_scrolling,
            snapshot,
        }
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let frame = prepaint.frame.clone();
        let theme = cx.theme().clone();
        let m = frame.metrics;
        let fonts = Fonts {
            mono: font(theme.mono_font_family.clone()),
            mono_italic: font(theme.mono_font_family.clone()).italic(),
            mono_bold: font(theme.mono_font_family.clone()).bold(),
            ui: font(theme.font_family.clone()),
            ui_bold: font(theme.font_family.clone()).bold(),
        };

        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            window.paint_quad(fill(bounds, theme.table));
            paint_body(prepaint, &fonts, &theme, window, cx);
            paint_gutter(prepaint, &fonts, &theme, window, cx);
            paint_header(prepaint, &fonts, &theme, window, cx);
            paint_scrollbars(prepaint, &theme, window);
        });

        // Cursor feedback for resize handles and clickable chart bars.
        let snap = &prepaint.snapshot;
        if snap.dragging_resize || matches!(snap.hover, Some(Hit::ResizeHandle(_))) {
            window.set_cursor_style(CursorStyle::ResizeLeftRight, &prepaint.hitbox);
        } else if matches!(snap.hover, Some(Hit::HeaderChart(_, Some(_)))) {
            // Chart bars filter when clicked.
            window.set_cursor_style(CursorStyle::PointingHand, &prepaint.hitbox);
        }
        let _ = m;
        register_listeners(self.state.clone(), prepaint.hitbox.clone(), window);
    }
}

fn register_listeners(state: Entity<GridState>, hitbox: Hitbox, window: &mut Window) {
    window.on_mouse_event({
        let state = state.clone();
        let hitbox = hitbox.clone();
        move |event: &MouseDownEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble && hitbox.is_hovered(window) {
                state.update(cx, |state, cx| state.on_mouse_down(event, window, cx));
            }
        }
    });
    window.on_mouse_event({
        let state = state.clone();
        let hitbox = hitbox.clone();
        move |event: &MouseMoveEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble {
                return;
            }
            let dragging = state.read(cx).drag.is_some();
            if dragging || hitbox.is_hovered(window) {
                state.update(cx, |state, cx| state.on_mouse_move(event, cx));
            } else {
                state.update(cx, |state, cx| state.on_hover_end(cx));
            }
        }
    });
    window.on_mouse_event({
        let state = state.clone();
        move |event: &MouseUpEvent, phase, _, cx| {
            if phase == DispatchPhase::Bubble && state.read(cx).drag.is_some() {
                state.update(cx, |state, cx| state.on_mouse_up(event, cx));
            }
        }
    });
    window.on_mouse_event({
        move |event: &ScrollWheelEvent, phase, window, cx| {
            if phase == DispatchPhase::Bubble && hitbox.should_handle_scroll(window) {
                let handled = state.update(cx, |state, cx| state.on_scroll(event, cx));
                if handled {
                    cx.stop_propagation();
                }
            }
        }
    });
}

fn text_run(len: usize, font: &Font, color: Hsla) -> TextRun {
    TextRun {
        len,
        font: font.clone(),
        color,
        background_color: None,
        underline: None,
        strikethrough: None,
    }
}

/// Paint single-line text inside `[left, left + width)`, truncated with an ellipsis.
#[allow(clippy::too_many_arguments)]
fn paint_label(
    text: &str,
    font: &Font,
    font_size: f32,
    color: Hsla,
    left: f32,
    top: f32,
    width: f32,
    line_height: f32,
    align_right: bool,
    window: &mut Window,
    cx: &mut App,
) {
    if text.is_empty() || width <= 2.0 {
        return;
    }
    let shape = |s: &str, window: &mut Window| {
        let owned: SharedString = s.to_string().into();
        window
            .text_system()
            .shape_line(owned, px(font_size), &[text_run(s.len(), font, color)], None)
    };
    let mut line = shape(text, window);
    let mut chars = text.chars().count();
    let mut attempts = 0;
    while f32::from(line.width) > width && chars > 1 && attempts < 4 {
        let ratio = width / f32::from(line.width);
        chars = ((chars as f32 * ratio).floor() as usize).clamp(1, chars - 1);
        let truncated = truncate_chars(text, chars).unwrap_or_else(|| text.to_string());
        line = shape(&truncated, window);
        attempts += 1;
    }
    let x = if align_right {
        left + (width - f32::from(line.width)).max(0.0)
    } else {
        left
    };
    let _ = line.paint(
        point(px(x.round()), px(top.round())),
        px(line_height),
        TextAlign::Left,
        None,
        window,
        cx,
    );
}

/// Cells use the monospace font, so truncation is a character count, not a measurement.
#[allow(clippy::too_many_arguments)]
fn paint_cell_text(
    text: &str,
    font: &Font,
    m: &Metrics,
    color: Hsla,
    left: f32,
    top: f32,
    width: f32,
    align_right: bool,
    window: &mut Window,
    cx: &mut App,
) {
    let inner = width - m.padding * 2.0;
    let max_chars = fitting_chars(inner, m.char_width);
    if max_chars == 0 {
        return;
    }
    let shown: SharedString = match truncate_chars(text, max_chars) {
        Some(truncated) => truncated.into(),
        None => text.to_string().into(),
    };
    let len = shown.len();
    let line = window
        .text_system()
        .shape_line(shown, px(m.font_size), &[text_run(len, font, color)], None);
    let x = if align_right {
        left + width - m.padding - f32::from(line.width)
    } else {
        left + m.padding
    };
    let baseline_top = top + (m.row_height - m.font_size * 1.3) / 2.0;
    let _ = line.paint(
        point(px(x.round()), px(baseline_top.round())),
        px(m.font_size * 1.3),
        TextAlign::Left,
        None,
        window,
        cx,
    );
}

fn paint_body(
    prepaint: &GridPrepaint,
    fonts: &Fonts,
    theme: &gpui_kit::component::Theme,
    window: &mut Window,
    cx: &mut App,
) {
    let frame = &prepaint.frame;
    let snap = &prepaint.snapshot;
    let m = frame.metrics;
    let body_left = frame.body_left();
    let body_top = frame.body_top();
    let body_width = frame.body_width();
    let body_height = frame.body_height();
    if body_height <= 0.0 {
        return;
    }
    let row_count = snap.row_count;
    let display_len = snap.display_len;
    let selection = snap.selection;
    let hover_row = match snap.hover {
        Some(Hit::Cell(pos)) => Some(pos.row),
        Some(Hit::RowHeader(row)) => Some(row),
        _ => None,
    };
    let top_fraction = ((frame.viewport.top - frame.viewport.top.floor()) * m.row_height as f64) as f32;
    let row_y = |row: u64| -> f32 {
        body_top + (row as f64 - frame.viewport.top.floor()) as f32 * m.row_height - top_fraction
    };

    // Rows end at the last column: the space to its right stays plain background.
    let content_width = match frame.layout.len() {
        0 => 0.0,
        len => {
            let last = len - 1;
            (frame.layout.left(last, frame.scroll_x) + frame.layout.width(last)).clamp(0.0, body_width)
        }
    };
    let rows_area = Bounds::new(point(px(body_left), px(body_top)), size(px(content_width), px(body_height)));

    // Row backgrounds, stripes and hover across the table's width.
    window.with_content_mask(Some(ContentMask { bounds: rows_area }), |window| {
        for row in prepaint.visible_rows.clone() {
            let y = row_y(row);
            let background = if hover_row == Some(row) {
                Some(theme.table_hover)
            } else if row % 2 == 1 {
                Some(theme.table_even)
            } else {
                None
            };
            if let Some(color) = background {
                window.paint_quad(fill(
                    Bounds::new(point(px(body_left), px(y)), size(px(body_width), px(m.row_height))),
                    color,
                ));
            }
        }
    });

    let pinned_width = frame.layout.pinned_width();
    let scrolling_area = Bounds::new(
        point(px(body_left + pinned_width), px(body_top)),
        size(px((body_width - pinned_width).max(0.0)), px(body_height)),
    );
    let pinned_area = Bounds::new(point(px(body_left), px(body_top)), size(px(pinned_width), px(body_height)));

    let paint_columns = |columns: std::ops::Range<usize>, area: Bounds<Pixels>, window: &mut Window, cx: &mut App| {
        window.with_content_mask(Some(ContentMask { bounds: area }), |window| {
            for display_ix in columns {
                let Some(col_snap) = snap.columns.get(&display_ix) else { continue };
                let column = col_snap.column;
                let info = &col_snap.info;
                let left = body_left + frame.layout.left(display_ix, frame.scroll_x);
                let width = frame.layout.width(display_ix);
                let align_right = info.kind.is_right_aligned();
                // Selection tint for the column span.
                for (row_ix, row) in prepaint.visible_rows.clone().enumerate() {
                    let y = row_y(row);
                    let selected = selection.is_some_and(|s| s.contains(row, display_ix, row_count, display_len));
                    if selected {
                        window.paint_quad(fill(
                            Bounds::new(point(px(left), px(y)), size(px(width), px(m.row_height))),
                            theme.selection.opacity(0.35),
                        ));
                    }
                    let cell_bounds_mask = Bounds::new(point(px(left), px(y)), size(px(width), px(m.row_height)));
                    match &col_snap.cells[row_ix] {
                        CellSnap::Text(text) => {
                            let color = if info.kind == ColumnKind::Boolean {
                                if text.as_ref() == "true" { theme.success } else { theme.muted_foreground }
                            } else {
                                theme.foreground
                            };
                            window.with_content_mask(Some(ContentMask { bounds: cell_bounds_mask }), |window| {
                                paint_cell_text(text, &fonts.mono, &m, color, left, y, width, align_right, window, cx);
                            });
                        }
                        CellSnap::Null => {
                            paint_cell_text("null", &fonts.mono_italic, &m, theme.muted_foreground.opacity(0.7), left, y, width, align_right, window, cx);
                        }
                        CellSnap::Loading => {
                            // Skeleton bar.
                            let bar_width = ((width - m.padding * 2.0) * (0.35 + ((row * 7 + column as u64 * 13) % 5) as f32 * 0.1)).max(0.0);
                            let bar_height = (m.font_size * 0.7).round();
                            let x = if align_right { left + width - m.padding - bar_width } else { left + m.padding };
                            window.paint_quad(
                                fill(
                                    Bounds::new(
                                        point(px(x), px(y + (m.row_height - bar_height) / 2.0)),
                                        size(px(bar_width), px(bar_height)),
                                    ),
                                    theme.skeleton,
                                )
                                .corner_radii(px(bar_height / 2.0)),
                            );
                        }
                        CellSnap::Failed => {
                            paint_cell_text("⚠", &fonts.mono, &m, theme.danger, left, y, width, false, window, cx);
                        }
                    }
                }
                // Column separator.
                window.paint_quad(fill(
                    Bounds::new(point(px(left + width - 1.0), px(body_top)), size(px(1.0), px(body_height))),
                    theme.table_row_border,
                ));
            }
        });
    };

    paint_columns(prepaint.visible_scrolling.clone(), scrolling_area, window, cx);
    if frame.layout.pinned() > 0 {
        // Pinned columns cover the scrolled content beneath them.
        window.with_content_mask(Some(ContentMask { bounds: pinned_area }), |window| {
            window.paint_quad(fill(pinned_area, theme.table));
            for row in prepaint.visible_rows.clone() {
                let y = row_y(row);
                let color = if hover_row == Some(row) {
                    Some(theme.table_hover)
                } else if row % 2 == 1 {
                    Some(theme.table_even)
                } else {
                    None
                };
                if let Some(color) = color {
                    window.paint_quad(fill(
                        Bounds::new(point(px(body_left), px(y)), size(px(pinned_width), px(m.row_height))),
                        color,
                    ));
                }
            }
        });
        paint_columns(0..frame.layout.pinned(), pinned_area, window, cx);
        window.paint_quad(fill(
            Bounds::new(point(px(body_left + pinned_width - 1.0), px(body_top)), size(px(2.0), px(body_height))),
            theme.border,
        ));
    }

    // Row separators.
    window.with_content_mask(Some(ContentMask { bounds: rows_area }), |window| {
        for row in prepaint.visible_rows.clone() {
            let y = row_y(row) + m.row_height - 1.0;
            window.paint_quad(fill(
                Bounds::new(point(px(body_left), px(y)), size(px(body_width), px(1.0))),
                theme.table_row_border.opacity(0.6),
            ));
        }
        // Active cell outline.
        if let Some(selection) = selection {
            let head = selection.head;
            if selection.kind == SelectionKind::Cells
                && head.col < frame.layout.len()
                && prepaint.visible_rows.contains(&head.row)
            {
                let left = body_left + frame.layout.left(head.col, frame.scroll_x);
                let pinned = frame.layout.is_pinned(head.col);
                let visible = pinned || left + frame.layout.width(head.col) > body_left + pinned_width;
                if visible {
                    let color = if snap.focused {
                        theme.primary
                    } else {
                        theme.muted_foreground
                    };
                    window.paint_quad(outline(
                        Bounds::new(
                            point(px(left), px(row_y(head.row))),
                            size(px(frame.layout.width(head.col)), px(m.row_height - 1.0)),
                        ),
                        color,
                        BorderStyle::Solid,
                    )
                    .border_widths(px(2.0)));
                }
            }
        }
    });

    if snap.empty_message {
        paint_label(
            "No rows",
            &fonts.ui,
            m.font_size,
            theme.muted_foreground,
            body_left,
            body_top + m.row_height,
            body_width.min(m.rem * 20.0),
            m.row_height,
            false,
            window,
            cx,
        );
    }
}

fn paint_gutter(
    prepaint: &GridPrepaint,
    fonts: &Fonts,
    theme: &gpui_kit::component::Theme,
    window: &mut Window,
    cx: &mut App,
) {
    let frame = &prepaint.frame;
    let snap = &prepaint.snapshot;
    let m = frame.metrics;
    let left = f32::from(frame.bounds.origin.x);
    let body_top = frame.body_top();
    let height = frame.body_height();
    let gutter = Bounds::new(point(px(left), px(body_top)), size(px(frame.gutter), px(height)));
    window.paint_quad(fill(gutter, theme.table_head));
    let selected_rows = snap.selection.map(|s| s.rows(snap.row_count));
    let top_fraction = ((frame.viewport.top - frame.viewport.top.floor()) * m.row_height as f64) as f32;
    window.with_content_mask(Some(ContentMask { bounds: gutter }), |window| {
        for row in prepaint.visible_rows.clone() {
            let y = body_top + (row as f64 - frame.viewport.top.floor()) as f32 * m.row_height - top_fraction;
            let selected = selected_rows.as_ref().is_some_and(|r| r.contains(&row));
            if selected {
                window.paint_quad(fill(
                    Bounds::new(point(px(left), px(y)), size(px(frame.gutter), px(m.row_height))),
                    theme.selection.opacity(0.5),
                ));
            }
            let color = if selected { theme.foreground } else { theme.muted_foreground.opacity(0.8) };
            paint_cell_text(&thousands(row), &fonts.mono, &m, color, left, y, frame.gutter, true, window, cx);
        }
    });
    window.paint_quad(fill(
        Bounds::new(point(px(left + frame.gutter - 1.0), px(body_top)), size(px(1.0), px(height))),
        theme.border,
    ));
}

fn paint_header(
    prepaint: &GridPrepaint,
    fonts: &Fonts,
    theme: &gpui_kit::component::Theme,
    window: &mut Window,
    cx: &mut App,
) {
    let frame = &prepaint.frame;
    let m = frame.metrics;
    let origin = frame.bounds.origin;
    let header = Bounds::new(origin, size(frame.bounds.size.width, px(m.header_height)));
    window.paint_quad(fill(header, theme.table_head));

    let body_left = frame.body_left();
    let pinned_width = frame.layout.pinned_width();
    let scrolling = Bounds::new(
        point(px(body_left + pinned_width), origin.y),
        size(px((frame.body_width() - pinned_width).max(0.0)), px(m.header_height)),
    );
    let pinned = Bounds::new(point(px(body_left), origin.y), size(px(pinned_width), px(m.header_height)));

    paint_header_cells(prepaint, prepaint.visible_scrolling.clone(), scrolling, fonts, theme, window, cx);
    if frame.layout.pinned() > 0 {
        window.paint_quad(fill(pinned, theme.table_head));
        paint_header_cells(prepaint, 0..frame.layout.pinned(), pinned, fonts, theme, window, cx);
        window.paint_quad(fill(
            Bounds::new(point(px(body_left + pinned_width - 1.0), origin.y), size(px(2.0), px(m.header_height))),
            theme.border,
        ));
    }

    // Corner above the row gutter.
    let corner = Bounds::new(origin, size(px(frame.gutter), px(m.header_height)));
    window.paint_quad(fill(corner, theme.table_head));
    paint_label(
        "#",
        &fonts.mono,
        m.small_font_size,
        theme.muted_foreground,
        f32::from(origin.x) + m.padding,
        f32::from(origin.y) + m.padding * 0.75,
        frame.gutter - m.padding * 2.0,
        m.small_font_size * 1.4,
        true,
        window,
        cx,
    );
    window.paint_quad(fill(
        Bounds::new(point(px(body_left - 1.0), origin.y), size(px(1.0), px(m.header_height))),
        theme.border,
    ));
    window.paint_quad(fill(
        Bounds::new(point(origin.x, origin.y + px(m.header_height - 1.0)), size(frame.bounds.size.width, px(1.0))),
        theme.border,
    ));
}

#[allow(clippy::too_many_arguments)]
fn paint_header_cells(
    prepaint: &GridPrepaint,
    columns: std::ops::Range<usize>,
    area: Bounds<Pixels>,
    fonts: &Fonts,
    theme: &gpui_kit::component::Theme,
    window: &mut Window,
    cx: &mut App,
) {
    let frame = &prepaint.frame;
    let snap = &prepaint.snapshot;
    let m = frame.metrics;
    let sort = &snap.sort;
    let selection = snap.selection;
    let top = f32::from(frame.bounds.origin.y);
    let (chart_top, chart_bottom) = frame.chart_rows();
    window.with_content_mask(Some(ContentMask { bounds: area }), |window| {
        for display_ix in columns {
            let Some(col_snap) = snap.columns.get(&display_ix) else { continue };
            let column = col_snap.column;
            let info = &col_snap.info;
            let left = frame.body_left() + frame.layout.left(display_ix, frame.scroll_x);
            let width = frame.layout.width(display_ix);
            let inner_left = left + m.padding;
            let inner_width = width - m.padding * 2.0;
            let cell = Bounds::new(point(px(left), px(top)), size(px(width), px(m.header_height)));

            let hovered = matches!(snap.hover, Some(Hit::Header(c)) | Some(Hit::HeaderChart(c, _)) if c == display_ix);
            let column_selected = selection.is_some_and(|s| {
                s.kind == SelectionKind::Columns && s.columns(snap.display_len).contains(&display_ix)
            });
            if column_selected {
                window.paint_quad(fill(cell, theme.selection.opacity(0.45)));
            } else if hovered {
                window.paint_quad(fill(cell, theme.table_hover));
            }

            // Name, with a sort arrow on the right.
            let sort_key = sort.iter().position(|k| k.column == info.name);
            let arrow_space = if sort_key.is_some() { m.font_size * 1.2 } else { 0.0 };
            let name_top = top + m.padding * 0.75;
            // Kind badge: a tinted chip with a short glyph (# Aa Dt …).
            let badge = kind_badge(info.kind);
            let kind = kind_color(info.kind, theme);
            let small_char = m.char_width * m.small_font_size / m.font_size;
            let badge_width = (badge.chars().count() as f32 * small_char + m.padding * 0.7).round();
            let badge_height = (m.font_size * 1.15).round();
            let badge_top = name_top + (m.font_size * 1.35 - badge_height) / 2.0;
            let show_badge = inner_width > badge_width * 3.0;
            let name_left = if show_badge { inner_left + badge_width + m.padding * 0.45 } else { inner_left };
            if show_badge {
                window.paint_quad(
                    fill(Bounds::new(point(px(inner_left), px(badge_top)), size(px(badge_width), px(badge_height))), kind.opacity(0.18))
                        .corner_radii(px((m.rem * 0.2).round())),
                );
                let pad = m.padding * 0.35;
                paint_label(badge, &fonts.mono_bold, m.small_font_size, kind, inner_left + pad, badge_top, badge_width - pad, badge_height, false, window, cx);
            }
            paint_label(
                &info.name,
                &fonts.ui_bold,
                m.font_size,
                theme.foreground,
                name_left,
                name_top,
                inner_left + inner_width - arrow_space - name_left,
                m.font_size * 1.35,
                false,
                window,
                cx,
            );
            if let Some(ix) = sort_key {
                let arrow = if sort[ix].descending { "↓" } else { "↑" };
                let label = if sort.len() > 1 { format!("{arrow}{}", ix + 1) } else { arrow.to_string() };
                paint_label(
                    &label,
                    &fonts.ui_bold,
                    m.font_size,
                    theme.primary,
                    inner_left + inner_width - arrow_space,
                    name_top,
                    arrow_space,
                    m.font_size * 1.35,
                    true,
                    window,
                    cx,
                );
            }

            // Type, and the null share on the right.
            let type_top = name_top + m.font_size * 1.35;
            let summary = col_snap.summary.clone();
            let nulls = match &summary {
                Some(SummaryState::Ready(s)) => null_label(s),
                _ => String::new(),
            };
            // Mono text at the small size: exact width from the cell font's advance.
            let null_width = if nulls.is_empty() { 0.0 } else { ((nulls.chars().count() as f32 + 0.5) * small_char).min(inner_width * 0.6) };
            paint_label(
                &info.type_label(),
                &fonts.mono,
                m.small_font_size,
                theme.muted_foreground,
                inner_left,
                type_top,
                inner_width - null_width - m.padding * 0.5,
                m.small_font_size * 1.3,
                false,
                window,
                cx,
            );
            if !nulls.is_empty() {
                paint_label(
                    &nulls,
                    &fonts.mono,
                    m.small_font_size,
                    null_color(theme),
                    inner_left + inner_width - null_width,
                    type_top,
                    null_width,
                    m.small_font_size * 1.3,
                    true,
                    window,
                    cx,
                );
            }

            if !snap.show_summaries {
                continue;
            }
            let chart = Bounds::new(
                point(px(inner_left), px(top + chart_top)),
                size(px(inner_width.max(0.0)), px(chart_bottom - chart_top)),
            );
            let footer_top = top + chart_bottom + m.padding * 0.2;
            match summary {
                Some(SummaryState::Ready(summary)) => {
                    let hovered_item = match snap.hover {
                        Some(Hit::HeaderChart(c, item)) if c == display_ix => item,
                        _ => None,
                    };
                    paint_chart(&summary, chart, hovered_item, chart_colors(info.kind, theme), window);
                    // Null share: a thin line along the bottom of the header.
                    let line = (m.rem * 0.15).max(2.0).round();
                    let line_top = top + m.header_height - line - (m.padding * 0.35).round();
                    let fraction = summary.null_fraction().clamp(0.0, 1.0) as f32;
                    window.paint_quad(
                        fill(Bounds::new(point(px(inner_left), px(line_top)), size(px(inner_width.max(0.0)), px(line))), theme.border.opacity(0.6))
                            .corner_radii(px(line / 2.0)),
                    );
                    if fraction > 0.0 {
                        let w = (inner_width * fraction).max(line);
                        window.paint_quad(
                            fill(Bounds::new(point(px(inner_left), px(line_top)), size(px(w), px(line))), null_color(theme))
                                .corner_radii(px(line / 2.0)),
                        );
                    }
                    if summary.preferred_chart() == parquetry_engine::ChartKind::None {
                        let text = chart_placeholder(&summary);
                        paint_label(&text, &fonts.ui, m.font_size, theme.muted_foreground, inner_left, top + chart_top + (chart_bottom - chart_top - m.font_size * 1.3) / 2.0, inner_width, m.font_size * 1.3, false, window, cx);
                    }
                    let (left_label, right_label) = footer_labels(&summary);
                    // The left label may use the whole width when there's nothing on the right.
                    let half = inner_width / 2.0;
                    let left_width = if right_label.is_empty() { inner_width } else { half };
                    paint_label(&left_label, &fonts.mono, m.small_font_size, theme.muted_foreground, inner_left, footer_top, left_width, m.small_font_size * 1.3, false, window, cx);
                    paint_label(&right_label, &fonts.mono, m.small_font_size, theme.muted_foreground, inner_left + half, footer_top, half, m.small_font_size * 1.3, true, window, cx);
                    if summary.sampled {
                        // A small dot marks sampled summaries; the tooltip explains.
                        let d = (m.rem * 0.3).round();
                        window.paint_quad(
                            fill(Bounds::new(point(px(inner_left + inner_width - d), px(top + chart_top)), size(px(d), px(d))), theme.info.opacity(0.7))
                                .corner_radii(px(d / 2.0)),
                        );
                    }
                }
                Some(SummaryState::Loading) | None => {
                    let bars = 12;
                    let slot = inner_width / bars as f32;
                    for i in 0..bars {
                        let h = (chart_bottom - chart_top) * (0.25 + 0.5 * (((i * 37 + column * 11) % 7) as f32 / 7.0));
                        window.paint_quad(fill(
                            Bounds::new(point(px(inner_left + slot * i as f32), px(top + chart_bottom - h)), size(px((slot - 2.0).max(1.0)), px(h))),
                            theme.skeleton,
                        ));
                    }
                }
                Some(SummaryState::Failed(_)) => {
                    paint_label("summary unavailable", &fonts.ui, m.small_font_size, theme.muted_foreground, inner_left, footer_top, inner_width, m.small_font_size * 1.3, false, window, cx);
                }
            }

            // Separator.
            window.paint_quad(fill(
                Bounds::new(point(px(left + width - 1.0), px(top)), size(px(1.0), px(m.header_height))),
                theme.border,
            ));
        }
    });
}

fn paint_scrollbars(prepaint: &GridPrepaint, theme: &gpui_kit::component::Theme, window: &mut Window) {
    let frame = &prepaint.frame;
    let snap = &prepaint.snapshot;
    let m = frame.metrics;
    let inset = (m.scrollbar * 0.2).round();
    let dragging_v = snap.dragging_vertical;
    let dragging_h = snap.dragging_horizontal;
    if let Some((track, thumb)) = frame.vertical_track {
        let active = dragging_v || matches!(snap.hover, Some(Hit::VerticalScrollbar { .. }));
        if active {
            window.paint_quad(fill(track, theme.scrollbar));
        }
        let bounds = Bounds::new(
            point(track.origin.x + px(inset), track.origin.y + px(thumb.start)),
            size(track.size.width - px(inset * 2.0), px(thumb.length)),
        );
        let color = if active { theme.scrollbar_thumb_hover } else { theme.scrollbar_thumb };
        window.paint_quad(fill(bounds, color).corner_radii(px(m.scrollbar / 2.0)));
    }
    if let Some((track, thumb)) = frame.horizontal_track {
        let active = dragging_h || matches!(snap.hover, Some(Hit::HorizontalScrollbar { .. }));
        if active {
            window.paint_quad(fill(track, theme.scrollbar));
        }
        let bounds = Bounds::new(
            point(track.origin.x + px(thumb.start), track.origin.y + px(inset)),
            size(px(thumb.length), track.size.height - px(inset * 2.0)),
        );
        let color = if active { theme.scrollbar_thumb_hover } else { theme.scrollbar_thumb };
        window.paint_quad(fill(bounds, color).corner_radii(px(m.scrollbar / 2.0)));
    }
}
