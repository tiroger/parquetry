//! UI integration tests: a real workspace window (headless) driving real data.
//!
//! These exercise every command the menus and shortcuts can send, so that a
//! re-entrant entity access or a missing handler shows up as a test failure
//! rather than a crash in someone's hands.

use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

use gpui_kit::component::{Root, WindowExt as _};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    Action, AnyWindowHandle, AppContext as _, Entity, Focusable as _, TestAppContext, WindowHandle, px, size,
};
use parquetry_engine::{Engine, EnginePaths, EngineSettings, Filter, FilterOp, SourceSpec};

use crate::actions::*;
use crate::app_state::AppState;
use crate::settings::Settings;
use crate::sql_panel::OpenDatasets;
use crate::workspace::Workspace;

struct Env {
    _dir: tempfile::TempDir,
    data: PathBuf,
    csv: PathBuf,
    other: PathBuf,
}

fn env() -> &'static Env {
    static ENV: OnceLock<Env> = OnceLock::new();
    ENV.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap();
        // Never touch the real settings file.
        unsafe { std::env::set_var("PARQUETRY_CONFIG_DIR", dir.path().join("config")) };
        let data = dir.path().join("sales.parquet");
        let other = dir.path().join("sales_v2.parquet");
        let csv = dir.path().join("people.csv");
        let conn = duckdb::Connection::open_in_memory().unwrap();
        conn.execute_batch(&format!(
            "COPY (SELECT i AS id, ['eu','us','apac'][1 + i % 3] AS region, (i * 7 % 1000) / 10.0 AS amount,
                          {{'k': i % 5}} AS meta, TIMESTAMP '2024-01-01' + INTERVAL (i) MINUTE AS ts
                   FROM range(20000) t(i)) TO '{}' (FORMAT parquet, ROW_GROUP_SIZE 5000);
             COPY (SELECT i AS id, ['eu','us','apac'][1 + i % 3] AS region, CASE WHEN i = 7 THEN -1 ELSE (i * 7 % 1000) / 10.0 END AS amount
                   FROM range(1, 20001) t(i)) TO '{}' (FORMAT parquet);",
            data.display(),
            other.display()
        ))
        .unwrap();
        std::fs::write(&csv, "name,age\nAda,36\nBob,41\n").unwrap();
        Env { _dir: dir, data, csv, other }
    })
}

fn setup(cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<Workspace>) {
    let env = env();
    let engine_dir = tempfile::tempdir().unwrap().keep();
    cx.update(|cx| {
        gpui_kit::init(cx);
        parquetry_grid::init(cx);
        let engine = Engine::new(EnginePaths::in_dir(&engine_dir), EngineSettings::default()).unwrap();
        cx.set_global(AppState {
            engine,
            settings: Settings::default(),
        });
        cx.set_global(OpenDatasets::default());
        crate::actions::bind_keys(cx);
        crate::register_global_actions(cx);
        crate::theme::apply(cx);
    });
    let _ = env;
    workspace_window(cx)
}

/// Another workspace window (globals already set up).
fn workspace_window(cx: &mut TestAppContext) -> (AnyWindowHandle, Entity<Workspace>) {
    let mut workspace = None;
    let handle: WindowHandle<Root> = cx.open_window(size(px(1280.), px(800.)), |window, cx| {
        let ws = cx.new(|cx| Workspace::new(window, cx));
        workspace = Some(ws.clone());
        Root::new(ws, window, cx)
    });
    let workspace = workspace.unwrap();
    let any: AnyWindowHandle = handle.into();
    let weak = workspace.downgrade();
    cx.update(|cx| cx.default_global::<crate::workspace::Workspaces>().0.push((any, weak)));
    (any, workspace)
}

/// Whether keyboard focus is somewhere inside the workspace.
fn focus_in_workspace(cx: &mut TestAppContext, handle: AnyWindowHandle, ws: &Entity<Workspace>) -> bool {
    cx.update_window(handle, |_, window, cx| ws.read(cx).focus_handle(cx).contains_focused(window, cx))
        .unwrap()
}

/// Pump GPUI and the engine's threads until `done` holds.
fn wait_until(cx: &mut TestAppContext, handle: AnyWindowHandle, what: &str, mut done: impl FnMut(&mut TestAppContext) -> bool) {
    cx.executor().allow_parking();
    for _ in 0..1500 {
        cx.update_window(handle, |_, window, cx| window.render_frame(cx)).unwrap();
        cx.run_until_parked();
        if done(cx) {
            return;
        }
        std::thread::sleep(Duration::from_millis(4));
    }
    panic!("timed out waiting for {what}");
}

fn open(cx: &mut TestAppContext, handle: AnyWindowHandle, ws: &Entity<Workspace>, path: &std::path::Path) {
    let before = cx.read(|cx| ws.read(cx).tab_count());
    cx.update_window(handle, |_, window, cx| {
        ws.update(cx, |w, cx| w.open(SourceSpec::new(path.to_string_lossy()), window, cx));
    })
    .unwrap();
    wait_until(cx, handle, "dataset to open", |cx| {
        cx.read(|cx| {
            let w = ws.read(cx);
            w.tab_count() > before && w.tab_kinds().last() == Some(&"dataset")
        })
    });
}

fn dispatch(cx: &mut TestAppContext, handle: AnyWindowHandle, action: Box<dyn Action>) {
    cx.update_window(handle, |_, window, cx| {
        window.dispatch_action(action, cx);
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| window.render_frame(cx)).unwrap();
}

fn close_dialogs(cx: &mut TestAppContext, handle: AnyWindowHandle) {
    cx.update_window(handle, |_, window, cx| {
        window.close_all_dialogs(cx);
        window.render_frame(cx);
    })
    .unwrap();
}

#[gpui_kit::test]
fn every_command_runs_without_crashing(cx: &mut TestAppContext) {
    let (handle, ws) = setup(cx);
    // With no document open, document commands must be harmless.
    for action in [Box::new(Find) as Box<dyn Action>, Box::new(AddFilter), Box::new(GoToRow), Box::new(GoToColumn), Box::new(Export), Box::new(Reload)] {
        dispatch(cx, handle, action);
    }
    open(cx, handle, &ws, &env().data);
    let commands: Vec<Box<dyn Action>> = vec![
        Box::new(ShowColumns),
        Box::new(ShowMetadata),
        Box::new(ShowSql),
        Box::new(ShowData),
        Box::new(ToggleInspector),
        Box::new(ToggleSummaries),
        Box::new(ToggleSummaries),
        Box::new(ExactSummaries),
        Box::new(Find),
        Box::new(GoToRow),
        Box::new(GoToColumn),
        Box::new(ShowValueCounts),
        Box::new(AddFilter),
        Box::new(Export),
        Box::new(ClearFilters),
        Box::new(Compare),
        Box::new(OpenSettings),
        Box::new(About),
        Box::new(ShowShortcuts),
        Box::new(ShowHelp),
        Box::new(OpenS3),
        Box::new(OpenUrl),
        Box::new(parquetry_grid::SelectAll),
        Box::new(parquetry_grid::MoveDown),
        Box::new(parquetry_grid::Inspect),
        Box::new(parquetry_grid::Copy),
        Box::new(NextTab),
        Box::new(PreviousTab),
    ];
    for action in commands {
        let name = action.name();
        dispatch(cx, handle, action);
        close_dialogs(cx, handle);
        // Give async work (summaries, copies) a moment to land.
        for _ in 0..5 {
            cx.run_until_parked();
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(focus_in_workspace(cx, handle, &ws), "focus left the workspace after {name}");
    }
    dispatch(cx, handle, Box::new(NewSqlConsole));
    assert_eq!(cx.read(|cx| ws.read(cx).tab_kinds()), vec!["dataset", "sql"]);
    dispatch(cx, handle, Box::new(CloseTab));
    dispatch(cx, handle, Box::new(Reload));
    wait_until(cx, handle, "reload", |cx| cx.read(|cx| ws.read(cx).tab_kinds() == vec!["dataset"]));
    dispatch(cx, handle, Box::new(CloseTab));
    assert_eq!(cx.read(|cx| ws.read(cx).tab_count()), 0);
}

#[gpui_kit::test]
fn filters_sort_and_search_update_the_grid(cx: &mut TestAppContext) {
    let (handle, ws) = setup(cx);
    open(cx, handle, &ws, &env().data);
    let doc = cx.read(|cx| ws.read(cx).document(0).unwrap());
    let grid = cx.read(|cx| doc.read(cx).grid.clone());
    assert_eq!(cx.read(|cx| grid.read(cx).row_count()), 20_000);

    cx.update_window(handle, |_, window, cx| {
        doc.update(cx, |d, cx| d.add_filter(Filter::new("region", FilterOp::Equals, "eu"), window, cx));
    })
    .unwrap();
    wait_until(cx, handle, "filter", |cx| cx.read(|cx| grid.read(cx).row_count() == 6_667));

    cx.update_window(handle, |_, window, cx| {
        doc.update(cx, |d, cx| d.sort_by("amount", Some(true), false, window, cx));
    })
    .unwrap();
    wait_until(cx, handle, "sort", |cx| {
        cx.read(|cx| grid.read(cx).view().is_some_and(|v| !v.spec.sort.is_empty()))
    });
    assert_eq!(cx.read(|cx| grid.read(cx).row_count()), 6_667);
    wait_until(cx, handle, "sorted rows", |cx| {
        cx.read(|cx| matches!(grid.read(cx).cell(0, 2), parquetry_grid::CellState::Loaded(Some(v)) if v.as_ref() == "99.9"))
    });

    // An invalid value is reported, and the previous view stays.
    cx.update_window(handle, |_, window, cx| {
        doc.update(cx, |d, cx| d.add_filter(Filter::new("amount", FilterOp::Greater, "lots"), window, cx));
    })
    .unwrap();
    wait_until(cx, handle, "error", |cx| cx.read(|cx| !doc.read(cx).is_busy()));
    assert_eq!(cx.read(|cx| grid.read(cx).row_count()), 6_667);

    cx.update_window(handle, |_, window, cx| {
        doc.update(cx, |d, cx| d.set_where("id < 100".into(), window, cx));
    })
    .unwrap();
    dispatch(cx, handle, Box::new(ClearFilters));
    wait_until(cx, handle, "cleared", |cx| cx.read(|cx| grid.read(cx).row_count() == 20_000 && !doc.read(cx).is_busy()));
    // Clearing filters keeps the sort.
    assert!(cx.read(|cx| !doc.read(cx).spec().has_filter() && !doc.read(cx).spec().sort.is_empty()));
}

#[gpui_kit::test]
fn sql_console_runs_queries_and_opens_results(cx: &mut TestAppContext) {
    let (handle, ws) = setup(cx);
    open(cx, handle, &ws, &env().data);
    dispatch(cx, handle, Box::new(NewSqlConsole));
    let panel = cx.read(|cx| ws.read(cx).sql_panel(1).unwrap());
    cx.update_window(handle, |_, window, cx| {
        panel.update(cx, |p, cx| {
            p.set_sql("SELECT region, count(*) AS n FROM sales GROUP BY 1 ORDER BY 1", window, cx);
            p.run(window, cx);
        });
    })
    .unwrap();
    wait_until(cx, handle, "query", |cx| cx.read(|cx| panel.read(cx).result().is_some() || panel.read(cx).error().is_some()));
    assert_eq!(cx.read(|cx| panel.read(cx).error().map(str::to_string)), None);
    assert_eq!(cx.read(|cx| panel.read(cx).result().unwrap().row_count), 3);
    cx.update(|cx| panel.update(cx, |p, cx| p.open_result(cx)));
    cx.run_until_parked();
    assert_eq!(cx.read(|cx| ws.read(cx).tab_kinds()), vec!["dataset", "sql", "dataset"]);

    // Errors are shown, not panicked on.
    cx.update_window(handle, |_, window, cx| {
        panel.update(cx, |p, cx| {
            p.set_sql("SELECT nope FROM sales", window, cx);
            p.run(window, cx);
        });
    })
    .unwrap();
    wait_until(cx, handle, "query error", |cx| cx.read(|cx| panel.read(cx).error().is_some()));
}

#[gpui_kit::test]
fn tabs_open_close_and_report_failures(cx: &mut TestAppContext) {
    let (handle, ws) = setup(cx);
    open(cx, handle, &ws, &env().data);
    open(cx, handle, &ws, &env().csv);
    open(cx, handle, &ws, &env().other);
    // Opening the same location again switches to it instead of duplicating.
    cx.update_window(handle, |_, window, cx| {
        ws.update(cx, |w, cx| w.open(SourceSpec::new(env().data.to_string_lossy()), window, cx));
    })
    .unwrap();
    assert_eq!(cx.read(|cx| (ws.read(cx).tab_count(), ws.read(cx).active_index())), (3, 0));

    cx.update_window(handle, |_, window, cx| ws.update(cx, |w, cx| w.test_close_tab(1, window, cx))).unwrap();
    assert_eq!(cx.read(|cx| ws.read(cx).tab_count()), 2);
    assert_eq!(cx.read(|cx| OpenDatasets::tables(cx).len()), 2);

    cx.update_window(handle, |_, window, cx| {
        ws.update(cx, |w, cx| w.open(SourceSpec::new("/definitely/not/here.parquet"), window, cx));
    })
    .unwrap();
    wait_until(cx, handle, "failure", |cx| cx.read(|cx| ws.read(cx).tab_kinds().last() == Some(&"failed")));
}

#[gpui_kit::test]
fn comparing_two_datasets(cx: &mut TestAppContext) {
    let (handle, ws) = setup(cx);
    open(cx, handle, &ws, &env().data);
    open(cx, handle, &ws, &env().other);
    let (left, right) = cx.read(|cx| {
        let w = ws.read(cx);
        (w.document(0).unwrap().read(cx).dataset.clone(), w.document(1).unwrap().read(cx).dataset.clone())
    });
    cx.update(|cx| ws.update(cx, |w, cx| w.test_compare(left, right, vec!["id".into()], cx)));
    let view = cx.read(|cx| ws.read(cx).compare_view(2).unwrap());
    wait_until(cx, handle, "comparison", |cx| cx.read(|cx| view.read(cx).result().is_some() || view.read(cx).error().is_some()));
    cx.read(|cx| {
        let r = view.read(cx).result().expect("comparison succeeded");
        assert_eq!(r.only_left.row_count, 1);
        assert_eq!(r.only_right.row_count, 1);
        assert_eq!(r.changed.as_ref().unwrap().row_count, 1);
        assert_eq!(r.only_left_columns.len(), 2);
    });
}

#[gpui_kit::test]
fn commands_still_work_when_focus_strays(cx: &mut TestAppContext) {
    let (handle, ws) = setup(cx);
    open(cx, handle, &ws, &env().data);
    for tab in [Box::new(ShowData) as Box<dyn Action>, Box::new(ShowColumns), Box::new(ShowMetadata), Box::new(ShowSql)] {
        dispatch(cx, handle, tab);
        // Park focus on the workspace root, as a dismissed popup can leave it.
        cx.update_window(handle, |_, window, cx| {
            let root = ws.read(cx).focus_handle(cx);
            window.focus(&root, cx);
        })
        .unwrap();
        dispatch(cx, handle, Box::new(GoToRow));
        let opened = cx.update_window(handle, |_, window, cx| window.has_active_dialog(cx)).unwrap();
        assert!(opened, "Go to Row didn't open");
        close_dialogs(cx, handle);
        // And with no focus at all.
        cx.update_window(handle, |_, window, cx| window.blur(cx)).unwrap();
        dispatch(cx, handle, Box::new(AddFilter));
        let opened = cx.update_window(handle, |_, window, cx| window.has_active_dialog(cx)).unwrap();
        assert!(opened, "Add Filter didn't open");
        close_dialogs(cx, handle);
    }
}

#[gpui_kit::test]
fn clicking_chart_bars_filters_and_drills_down(cx: &mut TestAppContext) {
    use parquetry_grid::SummaryState;
    let (handle, ws) = setup(cx);
    open(cx, handle, &ws, &env().data);
    let doc = cx.read(|cx| ws.read(cx).document(0).unwrap());
    let grid = cx.read(|cx| doc.read(cx).grid.clone());
    let ready = |cx: &mut TestAppContext, col: usize| {
        cx.read(|cx| matches!(grid.read(cx).summary(col), Some(SummaryState::Ready(_))))
    };
    wait_until(cx, handle, "summaries", |cx| ready(cx, 1) && ready(cx, 2));
    let click_bar = |cx: &mut TestAppContext, col: usize, bar: usize, bars: usize| {
        cx.update_window(handle, |_, window, cx| {
            let chart = grid.read(cx).chart_bounds(col).expect("chart painted");
            let x = chart.origin.x + chart.size.width * ((bar as f32 + 0.5) / bars as f32);
            let y = chart.origin.y + chart.size.height * 0.9;
            window.click_at("data-grid-root", gpui_kit::point(x, y), cx);
        })
        .unwrap();
    };
    let filters = |cx: &mut TestAppContext| cx.read(|cx| doc.read(cx).spec().filters.clone());

    // region: eu, us (6,667 rows each, alphabetical among ties), apac.
    click_bar(cx, 1, 1, 3);
    wait_until(cx, handle, "region filter", |cx| cx.read(|cx| grid.read(cx).row_count() == 6_667 && !doc.read(cx).is_busy()));
    assert_eq!(filters(cx), vec![Filter::new("region", FilterOp::Equals, "us")]);

    // Another bar of the same column replaces the filter instead of adding to it.
    wait_until(cx, handle, "region re-summarized", |cx| ready(cx, 1));
    click_bar(cx, 1, 0, 1);
    wait_until(cx, handle, "same bar again", |cx| cx.read(|cx| !doc.read(cx).is_busy()));
    assert_eq!(filters(cx).len(), 1);

    // amount: a histogram bar adds a range, and its rows are exactly the bar's.
    wait_until(cx, handle, "amount re-summarized", |cx| ready(cx, 2));
    let bin = cx.read(|cx| match grid.read(cx).summary(2) {
        Some(SummaryState::Ready(s)) => s.histogram[0].clone(),
        _ => unreachable!(),
    });
    click_bar(cx, 2, 0, 20);
    wait_until(cx, handle, "amount filter", |cx| cx.read(|cx| grid.read(cx).row_count() == bin.count && !doc.read(cx).is_busy()));
    let f = filters(cx);
    assert_eq!(f.len(), 3, "{f:?}");
    assert!(f.iter().any(|f| f.column == "amount" && f.op == FilterOp::GreaterOrEqual));
    assert!(f.iter().any(|f| f.column == "amount" && f.op == FilterOp::Less));

    // Drilling into the narrower histogram replaces the range.
    wait_until(cx, handle, "amount re-summarized", |cx| ready(cx, 2));
    click_bar(cx, 2, 19, 20);
    wait_until(cx, handle, "drill down", |cx| cx.read(|cx| !doc.read(cx).is_busy() && doc.read(cx).spec().filters.len() == 3 && grid.read(cx).row_count() < bin.count));
}

#[gpui_kit::test]
fn go_to_column_finds_and_reveals_hidden_columns(cx: &mut TestAppContext) {
    let (handle, ws) = setup(cx);
    // A wide table: the target is far off screen to the right.
    let path = env()._dir.path().join("wide.parquet");
    let columns: Vec<String> = (0..300).map(|i| format!("i + {i} AS c{i:03}")).collect();
    duckdb::Connection::open_in_memory()
        .unwrap()
        .execute_batch(&format!(
            "COPY (SELECT {}, 'x' AS needle_column FROM range(100) t(i)) TO '{}' (FORMAT parquet)",
            columns.join(", "),
            path.display()
        ))
        .unwrap();
    open(cx, handle, &ws, &path);
    let doc = cx.read(|cx| ws.read(cx).document(0).unwrap());
    let grid = cx.read(|cx| doc.read(cx).grid.clone());
    cx.update(|cx| grid.update(cx, |g, cx| g.set_hidden(300, true, cx)));

    dispatch(cx, handle, Box::new(GoToColumn));
    for _ in 0..4 {
        cx.update_window(handle, |_, window, cx| window.render_frame(cx)).unwrap();
        cx.run_until_parked();
    }
    cx.update_window(handle, |_, window, cx| window.input("needle", cx)).unwrap();
    cx.update_window(handle, |_, window, cx| window.render_frame(cx)).unwrap();
    cx.update_window(handle, |_, window, cx| window.render_frame(cx)).unwrap();
    cx.update_window(handle, |_, window, cx| window.press("enter", cx)).unwrap();
    cx.run_until_parked();
    cx.update_window(handle, |_, window, cx| window.render_frame(cx)).unwrap();

    cx.read(|cx| {
        let g = grid.read(cx);
        assert!(!g.is_hidden(300), "revealed columns are shown again");
        let display_ix = g.display_columns().iter().position(|&c| c == 300).unwrap();
        assert_eq!(g.selection().map(|s| s.head.col), Some(display_ix));
    });
    assert!(focus_in_workspace(cx, handle, &ws), "focus returns to the grid");
    assert!(cx.update_window(handle, |_, window, cx| !window.has_active_dialog(cx)).unwrap(), "dialog closed");
}

#[gpui_kit::test]
fn value_counts_keep_and_exclude_chosen_values(cx: &mut TestAppContext) {
    let (handle, ws) = setup(cx);
    open(cx, handle, &ws, &env().data);
    let doc = cx.read(|cx| ws.read(cx).document(0).unwrap());
    let grid = cx.read(|cx| doc.read(cx).grid.clone());
    let filters = |cx: &mut TestAppContext| cx.read(|cx| doc.read(cx).spec().filters.clone());
    let open_counts = |cx: &mut TestAppContext, values: usize| {
        cx.update_window(handle, |_, window, cx| doc.update(cx, |d, cx| d.open_value_counts(1, window, cx))).unwrap();
        wait_until(cx, handle, "value counts", |cx| {
            cx.update_window(handle, |_, window, _| window.try_find(("value-count", values - 1)).is_some()).unwrap()
        });
    };
    let click = |cx: &mut TestAppContext, id: gpui_kit::ElementId| {
        cx.update_window(handle, |_, window, cx| {
            window.click(id, cx);
            window.render_frame(cx);
        })
        .unwrap();
        cx.run_until_parked();
    };

    // Rows: eu (6,667), us (6,667), apac (6,666). Keep eu and apac.
    open_counts(cx, 3);
    click(cx, ("value-count", 0usize).into());
    click(cx, ("value-count", 2usize).into());
    click(cx, "keep-values".into());
    wait_until(cx, handle, "kept", |cx| cx.read(|cx| grid.read(cx).row_count() == 13_333 && !doc.read(cx).is_busy()));
    assert_eq!(filters(cx), vec![Filter::new("region", FilterOp::In, "eu, apac")]);
    assert!(cx.update_window(handle, |_, window, cx| !window.has_active_dialog(cx)).unwrap());

    // Excluding replaces the earlier value filter of the column: eu goes, and
    // us comes back.
    open_counts(cx, 2);
    click(cx, ("value-count", 0usize).into());
    click(cx, "exclude-values".into());
    wait_until(cx, handle, "excluded", |cx| cx.read(|cx| grid.read(cx).row_count() == 13_333 && !doc.read(cx).is_busy()));
    assert_eq!(filters(cx), vec![Filter::new("region", FilterOp::NotEquals, "eu")]);
}

#[gpui_kit::test]
fn sessions_save_and_restore_tabs_views_and_layout(cx: &mut TestAppContext) {
    use parquetry_engine::SortKey;
    let (handle, ws) = setup(cx);
    open(cx, handle, &ws, &env().data);
    let doc = cx.read(|cx| ws.read(cx).document(0).unwrap());
    let grid = cx.read(|cx| doc.read(cx).grid.clone());
    let spec = parquetry_engine::ViewSpec {
        filters: vec![Filter::new("region", FilterOp::Equals, "eu"), Filter::new("gone", FilterOp::IsNull, "")],
        sort: vec![SortKey::desc("amount")],
        ..Default::default()
    };
    let mut valid = spec.clone();
    valid.filters.pop();
    cx.update_window(handle, |_, window, cx| doc.update(cx, |d, cx| d.apply_spec(valid.clone(), window, cx))).unwrap();
    wait_until(cx, handle, "filtered", |cx| cx.read(|cx| grid.read(cx).row_count() == 6_667 && !doc.read(cx).is_busy()));
    cx.update(|cx| {
        grid.update(cx, |g, cx| {
            g.set_hidden(3, true, cx); // meta
            g.set_pinned(2, true, cx); // amount
            g.set_column_width(1, 17.0, cx);
            g.scroll_to_row(1_000, cx);
        })
    });
    dispatch(cx, handle, Box::new(ShowColumns));
    dispatch(cx, handle, Box::new(NewSqlConsole));
    let panel = cx.read(|cx| ws.read(cx).sql_panel(1).unwrap());
    cx.update_window(handle, |_, window, cx| panel.update(cx, |p, cx| p.set_sql("SELECT 7", window, cx))).unwrap();

    let mut session = cx.read(|cx| ws.read(cx).session(cx));
    assert_eq!(session.tabs.len(), 2);
    assert_eq!(session.active, 1);
    let crate::session::TabSession::Dataset(saved) = &mut session.tabs[0] else { panic!("dataset tab") };
    assert_eq!(saved.view, valid);
    assert_eq!(saved.tab, "columns");
    assert_eq!(saved.layout.hidden, vec!["meta".to_string()]);
    assert_eq!(saved.layout.order[0], "amount");
    assert_eq!(saved.layout.pinned, 1);
    // A filter on a column that has since disappeared is dropped on restore.
    saved.view = spec;
    assert_eq!(session.tabs[1], crate::session::TabSession::Sql { query: "SELECT 7".into() });

    // Survives being written and read back.
    let session: crate::session::WindowSession = serde_json::from_str(&serde_json::to_string(&session).unwrap()).unwrap();

    let (handle2, ws2) = workspace_window(cx);
    cx.update_window(handle2, |_, window, cx| ws2.update(cx, |w, cx| w.restore(session, window, cx))).unwrap();
    wait_until(cx, handle2, "restored dataset", |cx| cx.read(|cx| ws2.read(cx).tab_kinds() == vec!["dataset", "sql"]));
    let doc2 = cx.read(|cx| ws2.read(cx).document(0).unwrap());
    let grid2 = cx.read(|cx| doc2.read(cx).grid.clone());
    wait_until(cx, handle2, "restored view", |cx| cx.read(|cx| grid2.read(cx).row_count() == 6_667 && !doc2.read(cx).is_busy()));
    cx.read(|cx| {
        let d = doc2.read(cx);
        assert_eq!(d.spec(), &valid);
        let g = grid2.read(cx);
        assert!(g.is_hidden(3));
        assert!(g.is_pinned(2));
        assert_eq!(g.column_width(1), 17.0);
        assert_eq!(g.top_row(), 1_000);
        assert_eq!(ws2.read(cx).active_index(), 1);
        let panel = ws2.read(cx).sql_panel(1).unwrap();
        assert_eq!(panel.read(cx).query(cx), "SELECT 7");
        assert_eq!(d.session(cx).unwrap().tab, "columns");
    });

    // Reload keeps the view and layout.
    cx.update_window(handle2, |_, window, cx| ws2.update(cx, |w, cx| w.activate_for_test(0, window, cx))).unwrap();
    dispatch(cx, handle2, Box::new(Reload));
    wait_until(cx, handle2, "reloaded", |cx| {
        cx.read(|cx| {
            ws2.read(cx).tab_kinds()[0] == "dataset"
                && ws2.read(cx).document(0).is_some_and(|d| d.entity_id() != doc2.entity_id() && !d.read(cx).is_busy() && d.read(cx).grid.read(cx).row_count() == 6_667)
        })
    });
    let doc3 = cx.read(|cx| ws2.read(cx).document(0).unwrap());
    cx.read(|cx| {
        assert_eq!(doc3.read(cx).spec(), &valid);
        assert!(doc3.read(cx).grid.read(cx).is_pinned(2));
    });
}
