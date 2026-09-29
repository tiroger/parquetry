//! UI integration tests: a real Grid in a headless window over real DuckDB data.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    AnyWindowHandle, AppContext, Context, Entity, IntoElement, ParentElement, Render, ScrollDelta,
    Styled, TestAppContext, Window, component::Root, div, point, px, size,
};
use parquetry_engine::{Dataset, Engine, EnginePaths, EngineSettings, SortKey, SourceSpec, View, ViewSpec};
use parquetry_grid::{CellState, Grid, GridEvent, GridState, SummaryState};

struct Host {
    grid: Entity<GridState>,
}

impl Render for Host {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(Grid::new(&self.grid))
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    dataset: Dataset,
}

fn fixture(rows: u64) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("t.parquet");
    duckdb::Connection::open_in_memory()
        .unwrap()
        .execute_batch(&format!(
            "COPY (SELECT i AS id, 'name ' || i AS name, (i % 7)::DOUBLE / 2 AS score, ['a','b','c'][1 + i % 3] AS cat
                   FROM range({rows}) t(i)) TO '{}' (FORMAT parquet, ROW_GROUP_SIZE 100000)",
            path.display()
        ))
        .unwrap();
    let engine = Engine::new(EnginePaths::in_dir(dir.path()), EngineSettings::default()).unwrap();
    let dataset = Dataset::open(&engine, SourceSpec::new(path.to_string_lossy())).wait().unwrap();
    Fixture { _dir: dir, dataset }
}

fn open(cx: &mut TestAppContext, view: View) -> (AnyWindowHandle, Entity<GridState>) {
    cx.update(gpui_kit::init);
    cx.update(parquetry_grid::init);
    let mut grid = None;
    let handle = cx.open_window(size(px(1000.), px(600.)), |window, cx| {
        let state = cx.new(|cx| {
            let mut s = GridState::new(cx);
            s.set_view(Some(view), false, cx);
            s
        });
        grid = Some(state.clone());
        let host = cx.new(|_| Host { grid: state });
        Root::new(host, window, cx)
    });
    (handle.into(), grid.unwrap())
}

/// Pump GPUI and the engine's worker threads until `done` holds.
fn wait_until(cx: &mut TestAppContext, handle: AnyWindowHandle, grid: &Entity<GridState>, what: &str, done: impl Fn(&GridState) -> bool) {
    cx.executor().allow_parking();
    for _ in 0..1000 {
        cx.update_window(handle, |_, window, cx| window.render_frame(cx)).unwrap();
        cx.run_until_parked();
        if cx.read(|cx| done(grid.read(cx))) {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("timed out waiting for {what}");
}

fn loaded(state: &GridState, row: u64, column: usize) -> Option<String> {
    match state.cell(row, column) {
        CellState::Loaded(Some(text)) => Some(text.to_string()),
        _ => None,
    }
}

#[gpui_kit::test]
fn loads_visible_cells_and_summaries(cx: &mut TestAppContext) {
    let fx = fixture(10_000);
    let (handle, grid) = open(cx, View::identity(&fx.dataset));
    wait_until(cx, handle, &grid, "first cells", |s| loaded(s, 0, 0).is_some());
    cx.read(|cx| {
        let s = grid.read(cx);
        assert_eq!(loaded(s, 0, 1).as_deref(), Some("name 0"));
        assert_eq!(loaded(s, 5, 0).as_deref(), Some("5"));
        assert_eq!(s.row_count(), 10_000);
    });
    wait_until(cx, handle, &grid, "summaries", |s| matches!(s.summary(0), Some(SummaryState::Ready(_))));
    cx.read(|cx| {
        let Some(SummaryState::Ready(summary)) = grid.read(cx).summary(3) else { panic!("cat summary") };
        assert_eq!(summary.top_values.len(), 3);
    });
}

#[gpui_kit::test]
fn keyboard_navigation_selection_and_copy(cx: &mut TestAppContext) {
    let fx = fixture(10_000);
    let (handle, grid) = open(cx, View::identity(&fx.dataset));
    wait_until(cx, handle, &grid, "first cells", |s| loaded(s, 0, 0).is_some());
    cx.update_window(handle, |_, window, cx| {
        // Clicking a cell focuses the grid and selects it.
        window.click_at("data-grid-root", point(px(120.), px(140.)), cx);
        let (row, _) = grid.read(cx).active_cell().expect("clicked cell");
        assert!(row < 3, "row {row}");
        window.press("cmd-up", cx);
        window.press("down", cx);
        window.press("down", cx);
        window.press("right", cx);
        assert_eq!(grid.read(cx).active_cell(), Some((2, 1)));
        window.press("shift-down", cx);
        window.press("shift-right", cx);
        let (rows, cols) = grid.read(cx).selected_ranges().unwrap();
        assert_eq!(rows, 2..4);
        assert_eq!(cols, vec![1, 2]);
        window.press("cmd-c", cx);
    })
    .unwrap();
    wait_until(cx, handle, &grid, "clipboard", |_| true);
    for _ in 0..200 {
        cx.run_until_parked();
        if cx.read_from_clipboard().is_some() {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let text = cx.read_from_clipboard().and_then(|c| c.text()).expect("clipboard text");
    assert_eq!(text, "name 2\t1.0\nname 3\t1.5\n");

    cx.update_window(handle, |_, window, cx| {
        window.press("escape", cx);
        assert!(grid.read(cx).selection().is_none());
        window.press("cmd-a", cx);
        let (rows, cols) = grid.read(cx).selected_ranges().unwrap();
        assert_eq!(rows, 0..10_000);
        assert_eq!(cols.len(), 4);
    })
    .unwrap();
}

#[gpui_kit::test]
fn wheel_scrolling_and_jumping_to_the_end_of_millions_of_rows(cx: &mut TestAppContext) {
    let fx = fixture(3_000_000);
    let (handle, grid) = open(cx, View::identity(&fx.dataset));
    wait_until(cx, handle, &grid, "first cells", |s| loaded(s, 0, 0).is_some());
    cx.update_window(handle, |_, window, cx| {
        window.scroll("data-grid-root", ScrollDelta::Lines(point(0., -10.)), cx);
    })
    .unwrap();
    let top = cx.read(|cx| grid.read(cx).selection());
    assert!(top.is_none());
    cx.update_window(handle, |_, window, cx| {
        window.click_at("data-grid-root", point(px(120.), px(140.)), cx);
        window.press("cmd-down", cx);
        assert_eq!(grid.read(cx).active_cell().map(|c| c.0), Some(2_999_999));
    })
    .unwrap();
    wait_until(cx, handle, &grid, "last rows", |s| loaded(s, 2_999_999, 0).is_some());
    cx.read(|cx| assert_eq!(loaded(grid.read(cx), 2_999_999, 1).as_deref(), Some("name 2999999")));
}

#[gpui_kit::test]
fn header_click_emits_and_columns_hide_and_pin(cx: &mut TestAppContext) {
    let fx = fixture(1_000);
    let (handle, grid) = open(cx, View::identity(&fx.dataset));
    let events: Rc<RefCell<Vec<GridEvent>>> = Rc::default();
    let sink = events.clone();
    let _sub = cx.update(|cx| {
        cx.subscribe(&grid, move |_, event: &GridEvent, _| sink.borrow_mut().push(event.clone()))
    });
    wait_until(cx, handle, &grid, "first cells", |s| loaded(s, 0, 0).is_some());
    cx.update_window(handle, |_, window, cx| {
        // Header name area of the second column.
        let first_width = grid.read(cx).column_width(0) * f32::from(window.rem_size());
        window.click_at("data-grid-root", point(px(first_width + 80.), px(12.)), cx);
    })
    .unwrap();
    cx.run_until_parked();
    let clicked: Vec<usize> = events
        .borrow()
        .iter()
        .filter_map(|e| match e {
            GridEvent::HeaderClicked { column, .. } => Some(*column),
            _ => None,
        })
        .collect();
    assert_eq!(clicked, vec![1]);

    cx.update(|cx| {
        grid.update(cx, |s, cx| {
            s.set_hidden(1, true, cx);
            assert_eq!(s.display_columns(), &[0, 2, 3]);
            s.set_pinned(3, true, cx);
            assert_eq!(s.display_columns(), &[3, 0, 2]);
            assert_eq!(s.pinned_count(), 1);
            s.set_hidden(1, false, cx);
            assert_eq!(s.display_columns(), &[3, 0, 1, 2]);
            s.set_pinned(3, false, cx);
            assert_eq!(s.display_columns(), &[0, 1, 2, 3]);
            s.set_hidden(0, true, cx);
            s.show_all_columns(cx);
            assert_eq!(s.display_columns(), &[0, 1, 2, 3]);
        })
    });
}

#[gpui_kit::test]
fn new_views_keep_column_layout(cx: &mut TestAppContext) {
    let fx = fixture(1_000);
    let (handle, grid) = open(cx, View::identity(&fx.dataset));
    wait_until(cx, handle, &grid, "first cells", |s| loaded(s, 0, 0).is_some());
    cx.update(|cx| grid.update(cx, |s, cx| s.set_column_width(1, 20.0, cx)));
    let sorted = View::build(&fx.dataset, ViewSpec { sort: vec![SortKey::desc("id")], ..Default::default() })
        .wait()
        .unwrap();
    cx.update(|cx| grid.update(cx, |s, cx| s.set_view(Some(sorted), false, cx)));
    wait_until(cx, handle, &grid, "sorted cells", |s| loaded(s, 0, 0).is_some());
    cx.read(|cx| {
        let s = grid.read(cx);
        assert_eq!(s.column_width(1), 20.0);
        assert_eq!(loaded(s, 0, 0).as_deref(), Some("999"));
    });
}
