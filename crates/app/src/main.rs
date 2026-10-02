//! Parquetry: a fast viewer for Parquet and friends.

// Release builds on Windows are GUI apps: no console window.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod actions;
mod app_state;
mod compare_view;
mod dialogs;
mod document;
mod format;
// Used off macOS only; compiled everywhere so its tests run on every platform.
#[cfg_attr(target_os = "macos", allow(dead_code))]
mod instance;
mod notebook;
#[cfg(any(target_os = "macos", windows))]
mod notebook_window;
mod session;
mod settings;
mod sql_panel;
mod theme;
mod updater;
mod variant;
mod workspace;

#[cfg(test)]
mod ui_tests;

use std::path::PathBuf;

use futures::StreamExt as _;
use gpui_kit::*;
use parquetry_engine::{Engine, EnginePaths, SourceSpec};

use crate::actions::*;
use crate::app_state::AppState;
use crate::settings::{DEFAULT_FONT_SIZE, MAX_FONT_SIZE, MIN_FONT_SIZE, Settings};

fn main() {
    init_logging();
    let locations: Vec<String> = std::env::args()
        .skip(1)
        .filter(|a| !a.starts_with("-psn_"))
        .filter_map(|a| location_from_argument(&a))
        .collect();
    // Windows and Linux: a later launch hands its files to the running app.
    #[cfg(not(target_os = "macos"))]
    if instance::forward(&locations) {
        return;
    }

    let settings = Settings::load();
    let mut paths = EnginePaths::default_for_app(variant::APP_NAME);
    paths.bundled_extensions = bundled_extensions_dir();
    let engine = match Engine::new(paths, settings.engine.clone()) {
        Ok(engine) => engine,
        Err(error) => {
            eprintln!("Parquetry couldn't start its data engine: {error}");
            std::process::exit(1);
        }
    };
    let initial: Vec<SourceSpec> = locations.into_iter().map(SourceSpec::new).collect();

    let app = gpui_kit::application().with_assets(gpui_kit::assets::AllAssets);
    // Files opened from Finder, `open -a`, parquetry:// links, or (elsewhere) later
    // launches. An empty list asks for a new window.
    let (url_tx, mut url_rx) = futures::channel::mpsc::unbounded::<Vec<String>>();
    #[cfg(not(target_os = "macos"))]
    instance::listen(url_tx.clone());
    app.on_open_urls(move |urls| {
        let _ = url_tx.unbounded_send(urls);
    });
    app.on_reopen(|cx| {
        if cx.windows().is_empty() {
            workspace::open_window(Vec::new(), cx);
        }
    });

    app.run(move |cx| {
        gpui_kit::init(cx);
        parquetry_grid::init(cx);
        cx.set_global(AppState { engine, settings });
        cx.set_global(sql_panel::OpenDatasets::default());
        theme::apply(cx);
        actions::bind_keys(cx);
        register_global_actions(cx);
        updater::start();
        actions::set_menus(cx);

        cx.spawn(async move |cx: &mut AsyncApp| {
            while let Some(urls) = url_rx.next().await {
                let specs: Vec<SourceSpec> = urls
                    .iter()
                    .filter_map(|u| location_from_url(u))
                    .map(SourceSpec::new)
                    .collect();
                cx.update(|cx| {
                    cx.activate(true);
                    if !specs.is_empty() {
                        workspace::open_in_front_window(specs, cx);
                    } else if urls.is_empty() {
                        workspace::open_window(Vec::new(), cx);
                    }
                });
            }
        })
        .detach();

        // Notebook windows' marimo servers stop with Parquetry.
        cx.on_app_quit(|_| {
            notebook::MarimoServer::stop_all();
            async {}
        })
        .detach();
        let session = session::SessionKeeper::start(cx);
        if initial.is_empty() && session::should_restore(cx) && !session.windows.is_empty() {
            // Front window last, so it ends up in front.
            for saved in session.windows.into_iter().rev() {
                if let Some(workspace) = workspace::open_window(Vec::new(), cx).and_then(|w| w.upgrade()) {
                    workspace::restore_window(&workspace, saved, cx);
                }
            }
        } else {
            workspace::open_window(initial, cx);
        }
        cx.activate(true);
    });
}

fn register_global_actions(cx: &mut App) {
    cx.on_action(|_: &Quit, cx| cx.quit());
    cx.on_action(|_: &CheckForUpdates, _| updater::check_for_updates());
    cx.on_action(|_: &NewWindow, cx| {
        workspace::open_window(Vec::new(), cx);
    });
    cx.on_action(|_: &ZoomIn, cx| zoom(cx, 1.0));
    cx.on_action(|_: &ZoomOut, cx| zoom(cx, -1.0));
    cx.on_action(|_: &ResetZoom, cx| AppState::update_settings(cx, |s| s.font_size = DEFAULT_FONT_SIZE));
    cx.on_action(|_: &UseLightAppearance, cx| AppState::update_settings(cx, |s| s.appearance = settings::Appearance::Light));
    cx.on_action(|_: &UseDarkAppearance, cx| AppState::update_settings(cx, |s| s.appearance = settings::Appearance::Dark));
    cx.on_action(|_: &UseSystemAppearance, cx| AppState::update_settings(cx, |s| s.appearance = settings::Appearance::System));
    cx.on_action(|_: &UseNavyOakTheme, cx| AppState::update_settings(cx, |s| s.dark_theme = settings::DarkTheme::NavyOak));
    cx.on_action(|_: &UseSlateTheme, cx| AppState::update_settings(cx, |s| s.dark_theme = settings::DarkTheme::Slate));
    cx.on_action(|_: &OpenNotebook, cx| {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Open Notebook".into()),
        });
        cx.spawn(async move |cx: &mut AsyncApp| {
            let Some(path) = picked.await.ok().and_then(|r| r.ok()).flatten().and_then(|p| p.into_iter().next()) else {
                return;
            };
            cx.update(|cx| open_notebook(path, cx));
        })
        .detach();
    });
    cx.on_action(|action: &OpenRecentNotebook, cx| {
        let recent = AppState::settings(cx).existing_recent_notebooks();
        if let Some(path) = recent.get(action.0) {
            open_notebook(PathBuf::from(path), cx);
        }
    });
    cx.on_action(|_: &ClearRecentNotebooks, cx| {
        AppState::update_settings(cx, |s| s.recent_notebooks.clear());
        actions::set_menus(cx);
    });
    cx.on_action(|_: &ClearRecents, cx| {
        AppState::update_settings(cx, |s| s.recents.clear());
        actions::set_menus(cx);
    });
    // Commands that need a window fall back to a new one when none is open.
    cx.on_action(|_: &OpenFile, cx| with_new_window(Box::new(OpenFile), cx));
    cx.on_action(|_: &OpenFolder, cx| with_new_window(Box::new(OpenFolder), cx));
    cx.on_action(|_: &OpenS3, cx| with_new_window(Box::new(OpenS3), cx));
    cx.on_action(|_: &OpenUrl, cx| with_new_window(Box::new(OpenUrl), cx));
    cx.on_action(|_: &NewSqlConsole, cx| with_new_window(Box::new(NewSqlConsole), cx));
    cx.on_action(|_: &OpenSettings, cx| with_new_window(Box::new(OpenSettings), cx));
    cx.on_action(|_: &About, cx| with_new_window(Box::new(About), cx));
    cx.on_action(|action: &OpenRecent, cx| with_new_window(Box::new(action.clone()), cx));
    // Window and document commands: route to the front window when focus has
    // wandered outside its content (never opens a new window).
    macro_rules! to_front {
        ($($action:ident),* $(,)?) => {$(
            cx.on_action(|_: &$action, cx| {
                workspace::dispatch_to_front_workspace(Box::new($action), cx);
            });
        )*};
    }
    to_front!(
        CloseTab, NextTab, PreviousTab, Reload, Compare, Export, Find, GoToRow, GoToColumn, ShowValueCounts, OpenInMarimo, CopyAsSql, CopyAsPolars, CopyAsPandas, AddFilter, ClearFilters,
        ShowData, ShowColumns, ShowMetadata, ShowSql, ToggleInspector, ToggleSummaries, ExactSummaries,
        ShowShortcuts, ShowHelp,
    );
}

fn open_notebook(path: PathBuf, cx: &mut App) {
    if path.extension().and_then(|e| e.to_str()) != Some("py") {
        notebook::notify_error(cx, "Not a notebook", "marimo notebooks are .py files.");
        return;
    }
    if let Err(error) = notebook::show_notebook(path, cx) {
        notebook::notify_error(cx, "Couldn’t open the notebook", &format!("{error:#}"));
    }
}

/// Global handlers only run when no element in a window handled the action: either
/// no window is open, or focus isn't inside a workspace (e.g. just after a dialog
/// closed). Deliver the action to the frontmost workspace, or a new window.
fn with_new_window(action: Box<dyn Action>, cx: &mut App) {
    if workspace::dispatch_to_front_workspace(action.boxed_clone(), cx) {
        return;
    }
    if let Some(workspace) = workspace::open_window(Vec::new(), cx) {
        let registry = cx.default_global::<workspace::Workspaces>();
        if let Some((handle, _)) = registry.0.iter().find(|(_, w)| w.entity_id() == workspace.entity_id()).cloned() {
            let _ = handle.update(cx, |_, window, cx| window.dispatch_action(action, cx));
        }
    }
}

fn zoom(cx: &mut App, delta: f32) {
    AppState::update_settings(cx, |s| s.font_size = (s.font_size + delta).clamp(MIN_FONT_SIZE, MAX_FONT_SIZE));
}

/// `parquetry://open?url=…` (`parquetry-preview://` for the preview), `file:///…`
/// or a plain path → a location to open.
fn location_from_url(url: &str) -> Option<String> {
    if let Some(rest) = url.strip_prefix("parquetry://").or_else(|| url.strip_prefix("parquetry-preview://")) {
        let query = rest.split_once('?').map(|(_, q)| q).unwrap_or("");
        for pair in query.split('&') {
            if let Some(value) = pair.strip_prefix("url=") {
                let decoded = percent_decode(value);
                return (!decoded.is_empty()).then_some(decoded);
            }
        }
        return None;
    }
    if let Some(path) = url.strip_prefix("file://") {
        let path = percent_decode(path);
        let path = path.strip_prefix("localhost").unwrap_or(&path).to_string();
        // file:///C:/data/x.parquet -> C:/data/x.parquet
        if let Some(rest) = path.strip_prefix('/')
            && has_drive_letter(rest)
        {
            return Some(rest.trim_end_matches('/').to_string());
        }
        return Some(path.trim_end_matches('/').to_string()).filter(|p| !p.is_empty()).or(Some("/".into()));
    }
    location_from_argument(url)
}

/// Command-line arguments: relative paths become absolute.
fn location_from_argument(arg: &str) -> Option<String> {
    let arg = arg.trim();
    if arg.is_empty() || arg.starts_with('-') {
        return None;
    }
    if parquetry_engine::is_remote(arg) || arg.starts_with('/') || arg.starts_with('~') || has_drive_letter(arg) || arg.starts_with(r"\\") {
        return Some(arg.to_string());
    }
    let absolute = std::env::current_dir().map(|d| d.join(arg)).unwrap_or_else(|_| PathBuf::from(arg));
    Some(absolute.to_string_lossy().into_owned())
}

/// `C:\…` or `C:/…` (Windows absolute paths).
fn has_drive_letter(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() >= 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && (bytes[2] == b'\\' || bytes[2] == b'/')
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                match u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or("zz"), 16) {
                    Ok(value) => {
                        out.push(value);
                        i += 3;
                        continue;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            b'+' => out.push(b' '),
            b => out.push(b),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Extensions shipped in the app bundle (`Contents/Resources/duckdb_extensions`).
fn bundled_extensions_dir() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let dir = exe.parent()?.parent()?.join("Resources").join("duckdb_extensions");
    dir.is_dir().then_some(dir)
}

fn init_logging() {
    let level = std::env::var("PARQUETRY_LOG")
        .ok()
        .and_then(|l| l.parse().ok())
        .unwrap_or(log::LevelFilter::Warn);
    struct Logger(log::LevelFilter);
    impl log::Log for Logger {
        fn enabled(&self, metadata: &log::Metadata) -> bool {
            metadata.level() <= self.0
        }
        fn log(&self, record: &log::Record) {
            if self.enabled(record.metadata()) {
                eprintln!("[{}] {}: {}", record.level(), record.target(), record.args());
            }
        }
        fn flush(&self) {}
    }
    let logger: &'static Logger = Box::leak(Box::new(Logger(level)));
    let _ = log::set_logger(logger);
    log::set_max_level(level);
}

#[cfg(test)]
mod tests {
    use super::{location_from_url, percent_decode};

    #[test]
    fn urls() {
        assert_eq!(percent_decode("a%20b+c%2Fd"), "a b c/d");
        assert_eq!(
            location_from_url("parquetry://open?url=s3%3A%2F%2Fbucket%2Fk.parquet").as_deref(),
            Some("s3://bucket/k.parquet")
        );
        assert_eq!(
            location_from_url("file:///Users/me/My%20File.parquet").as_deref(),
            Some("/Users/me/My File.parquet")
        );
        assert_eq!(location_from_url("file:///Users/me/dir/").as_deref(), Some("/Users/me/dir"));
        assert_eq!(location_from_url("parquetry://open").as_deref(), None);
        assert_eq!(location_from_url("parquetry-preview://open?url=s3%3A%2F%2Fb%2Fk").as_deref(), Some("s3://b/k"));
        assert_eq!(location_from_url("s3://b/k").as_deref(), Some("s3://b/k"));
        assert_eq!(location_from_url("file:///C:/data/My%20File.parquet").as_deref(), Some("C:/data/My File.parquet"));
        assert_eq!(location_from_url(r"C:\data\x.parquet").as_deref(), Some(r"C:\data\x.parquet"));
        assert_eq!(location_from_url(r"\\server\share\x.parquet").as_deref(), Some(r"\\server\share\x.parquet"));
    }
}
