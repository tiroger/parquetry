//! Application commands, their key bindings and the native menu bar.

use gpui_kit::*;

use crate::app_state::AppState;

gpui_kit::actions!(
    parquetry,
    [
        About,
        CheckForUpdates,
        OpenSettings,
        Quit,
        NewWindow,
        OpenFile,
        OpenFolder,
        OpenS3,
        OpenUrl,
        NewSqlConsole,
        CloseTab,
        NextTab,
        PreviousTab,
        Reload,
        Export,
        Compare,
        Find,
        GoToRow,
        GoToColumn,
        ShowValueCounts,
        OpenInMarimo,
        CopyAsSql,
        CopyAsPolars,
        CopyAsPandas,
        AddFilter,
        ClearFilters,
        ShowData,
        ShowColumns,
        ShowMetadata,
        ShowSql,
        ToggleInspector,
        ToggleSummaries,
        ExactSummaries,
        ZoomIn,
        ZoomOut,
        ResetZoom,
        UseLightAppearance,
        UseDarkAppearance,
        UseSystemAppearance,
        UseNavyOakTheme,
        UseSlateTheme,
        RunQuery,
        ShowHelp,
        ShowShortcuts,
        ClearRecents,
    ]
);

/// Open a recent location (index into the recents list).
#[derive(Clone, PartialEq, Debug, gpui_kit::Action)]
#[action(namespace = parquetry, no_json)]
pub struct OpenRecent(pub usize);

/// Shortcuts that differ between platforms. Everything else uses `secondary-`,
/// which is ⌘ on macOS and Ctrl elsewhere.
#[cfg(target_os = "macos")]
mod keys {
    pub const TOGGLE_INSPECTOR: &str = "cmd-alt-i";
    pub const NEW_SQL_CONSOLE: &str = "cmd-alt-n";
    /// Shown in the shortcut list; `secondary-r` works everywhere.
    pub const RELOAD: &str = "cmd-r";
}
// Ctrl+Alt is AltGr on many keyboard layouts, so avoid it off macOS.
#[cfg(not(target_os = "macos"))]
mod keys {
    pub const TOGGLE_INSPECTOR: &str = "ctrl-shift-i";
    pub const NEW_SQL_CONSOLE: &str = "ctrl-shift-n";
    pub const RELOAD: &str = "f5";
}

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("secondary-,", OpenSettings, None),
        KeyBinding::new("secondary-q", Quit, None),
        KeyBinding::new("secondary-n", NewWindow, None),
        KeyBinding::new("secondary-o", OpenFile, None),
        KeyBinding::new("secondary-shift-o", OpenS3, None),
        KeyBinding::new("secondary-l", OpenUrl, None),
        KeyBinding::new(keys::NEW_SQL_CONSOLE, NewSqlConsole, None),
        KeyBinding::new("secondary-w", CloseTab, None),
        KeyBinding::new("ctrl-tab", NextTab, None),
        KeyBinding::new("ctrl-shift-tab", PreviousTab, None),
        KeyBinding::new("secondary-shift-]", NextTab, None),
        KeyBinding::new("secondary-shift-[", PreviousTab, None),
        KeyBinding::new("secondary-r", Reload, None),
        KeyBinding::new("secondary-shift-e", Export, None),
        KeyBinding::new("secondary-f", Find, None),
        KeyBinding::new("secondary-g", GoToRow, None),
        KeyBinding::new("secondary-p", GoToColumn, None),
        KeyBinding::new("secondary-shift-m", OpenInMarimo, None),
        KeyBinding::new("secondary-shift-f", AddFilter, None),
        KeyBinding::new("secondary-shift-k", ClearFilters, None),
        KeyBinding::new("secondary-1", ShowData, None),
        KeyBinding::new("secondary-2", ShowColumns, None),
        KeyBinding::new("secondary-3", ShowMetadata, None),
        KeyBinding::new("secondary-4", ShowSql, None),
        KeyBinding::new(keys::TOGGLE_INSPECTOR, ToggleInspector, None),
        KeyBinding::new("secondary-=", ZoomIn, None),
        KeyBinding::new("secondary-+", ZoomIn, None),
        KeyBinding::new("secondary--", ZoomOut, None),
        KeyBinding::new("secondary-0", ResetZoom, None),
        KeyBinding::new("secondary-/", ShowShortcuts, None),
        // Run SQL from inside the editor; registered after gpui_kit::init so it wins.
        KeyBinding::new("secondary-enter", RunQuery, Some("SqlEditor > Input")),
        KeyBinding::new("secondary-enter", RunQuery, Some("SqlEditor")),
    ]);
    #[cfg(not(target_os = "macos"))]
    cx.bind_keys([KeyBinding::new("f5", Reload, None)]);
}

/// A key binding (`"secondary-shift-o"`, space-separated for sequences) the way
/// this platform writes it: ⇧⌘O on macOS, Ctrl+Shift+O elsewhere.
pub fn key_label(binding: &str) -> String {
    binding
        .split(' ')
        .filter_map(|key| Keystroke::parse(key).ok())
        .map(|key| gpui_kit::component::kbd::Kbd::format(&key))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Build the menu bar: the native one on macOS, drawn in each window elsewhere
/// (see `workspace`). Call again when recents change.
pub fn set_menus(cx: &mut App) {
    use gpui_kit::component::input::{Copy, Cut, Paste, Redo, SelectAll, Undo};
    let recents: Vec<MenuItem> = AppState::settings(cx)
        .recents
        .iter()
        .take(15)
        .enumerate()
        .map(|(ix, recent)| MenuItem::action(crate::format::display_path(&recent.location), OpenRecent(ix)))
        .collect();
    let mut recent_items = recents;
    if !recent_items.is_empty() {
        recent_items.push(MenuItem::separator());
        recent_items.push(MenuItem::action("Clear Menu", ClearRecents));
    }
    // macOS has an application menu; elsewhere its items move to File and Help.
    let mac = cfg!(target_os = "macos");
    let app_menu = Menu::new(crate::variant::APP_NAME).items([
        MenuItem::action(format!("About {}", crate::variant::APP_NAME), About),
        MenuItem::action("Check for Updates…", CheckForUpdates).disabled(!crate::updater::is_available()),
        MenuItem::separator(),
        MenuItem::action("Settings…", OpenSettings),
        MenuItem::separator(),
        MenuItem::action(format!("Quit {}", crate::variant::APP_NAME), Quit),
    ]);
    let mut file_items = vec![
        MenuItem::action("New Window", NewWindow),
        MenuItem::action("New SQL Console", NewSqlConsole),
        MenuItem::separator(),
        MenuItem::action("Open…", OpenFile),
        MenuItem::action("Open Folder…", OpenFolder),
        MenuItem::action("Open S3 Location…", OpenS3),
        MenuItem::action("Open URL or Path…", OpenUrl),
        MenuItem::submenu(Menu::new("Open Recent").items(recent_items)),
        MenuItem::separator(),
        MenuItem::action("Export…", Export),
        MenuItem::action("Open in marimo", OpenInMarimo),
        MenuItem::action("Compare…", Compare),
        MenuItem::action("Reload", Reload),
        MenuItem::separator(),
        MenuItem::action("Close Tab", CloseTab),
    ];
    if !mac {
        file_items.extend([
            MenuItem::separator(),
            MenuItem::action("Settings…", OpenSettings),
            MenuItem::separator(),
            MenuItem::action("Exit", Quit),
        ]);
    }
    let mut help_items = vec![
        MenuItem::action("Keyboard Shortcuts", ShowShortcuts),
        MenuItem::action("Parquetry Help", ShowHelp),
    ];
    if !mac {
        if crate::updater::is_available() {
            help_items.push(MenuItem::separator());
            help_items.push(MenuItem::action("Check for Updates…", CheckForUpdates));
        }
        help_items.push(MenuItem::separator());
        help_items.push(MenuItem::action(format!("About {}", crate::variant::APP_NAME), About));
    }
    let mut menus = Vec::new();
    if mac {
        menus.push(app_menu);
    }
    menus.extend([
        Menu::new("File").items(file_items),
        Menu::new("Edit").items([
            MenuItem::os_action("Undo", Undo, OsAction::Undo),
            MenuItem::os_action("Redo", Redo, OsAction::Redo),
            MenuItem::separator(),
            MenuItem::os_action("Cut", Cut, OsAction::Cut),
            MenuItem::os_action("Copy", Copy, OsAction::Copy),
            MenuItem::action("Copy with Headers", parquetry_grid::CopyWithHeaders),
            MenuItem::submenu(Menu::new("Copy View as Code").items([
                MenuItem::action("SQL (DuckDB)", CopyAsSql),
                MenuItem::action("Polars", CopyAsPolars),
                MenuItem::action("pandas", CopyAsPandas),
            ])),
            MenuItem::os_action("Paste", Paste, OsAction::Paste),
            MenuItem::os_action("Select All", SelectAll, OsAction::SelectAll),
            MenuItem::separator(),
            MenuItem::action("Find…", Find),
            MenuItem::action("Go to Row…", GoToRow),
            MenuItem::action("Go to Column…", GoToColumn),
            MenuItem::action("Add Filter…", AddFilter),
            MenuItem::action("Clear Filters", ClearFilters),
        ]),
        Menu::new("View").items([
            MenuItem::action("Data", ShowData),
            MenuItem::action("Columns", ShowColumns),
            MenuItem::action("Metadata", ShowMetadata),
            MenuItem::action("SQL", ShowSql),
            MenuItem::separator(),
            MenuItem::action("Toggle Inspector", ToggleInspector),
            MenuItem::action("Toggle Column Summaries", ToggleSummaries),
            MenuItem::action("Compute Exact Summaries", ExactSummaries),
            MenuItem::action("Value Counts…", ShowValueCounts),
            MenuItem::separator(),
            MenuItem::action("Zoom In", ZoomIn),
            MenuItem::action("Zoom Out", ZoomOut),
            MenuItem::action("Actual Size", ResetZoom),
            MenuItem::separator(),
            MenuItem::submenu(Menu::new("Appearance").items([
                MenuItem::action("Match System", UseSystemAppearance),
                MenuItem::action("Light", UseLightAppearance),
                MenuItem::action("Dark", UseDarkAppearance),
                MenuItem::separator(),
                MenuItem::action("Navy & Oak (dark)", UseNavyOakTheme),
                MenuItem::action("Slate (dark)", UseSlateTheme),
            ])),
        ]),
        Menu::new("Window").items([
            MenuItem::action("Show Next Tab", NextTab),
            MenuItem::action("Show Previous Tab", PreviousTab),
        ]),
        Menu::new("Help").items(help_items),
    ]);
    cx.set_menus(menus);
    crate::workspace::reload_menu_bars(cx);
}

/// The shortcut list shown in the help dialog, labelled for this platform.
pub fn shortcut_list() -> Vec<(String, &'static str)> {
    let k = key_label;
    let (arrows, inspect) = if cfg!(target_os = "macos") {
        ("Arrows, ⇧ Arrows", format!("{} or Space", k("enter")))
    } else {
        ("Arrows, Shift+Arrows", format!("{} or Space", k("enter")))
    };
    vec![
        (k("secondary-o"), "Open file"),
        (k("secondary-shift-o"), "Open S3 location"),
        (k("secondary-l"), "Open URL or path"),
        (k("secondary-f"), "Search all columns"),
        (k("secondary-shift-f"), "Add filter"),
        (k("secondary-shift-k"), "Clear filters"),
        (k("secondary-g"), "Go to row"),
        (k("secondary-p"), "Go to column"),
        (format!("{} – {}", k("secondary-1"), k("secondary-4")), "Data, Columns, Metadata, SQL"),
        (k(keys::TOGGLE_INSPECTOR), "Toggle inspector"),
        (k("secondary-enter"), "Run SQL"),
        (format!("{} / {}", k("secondary-c"), k("secondary-shift-c")), "Copy selection / with headers"),
        (k("secondary-a"), "Select all cells"),
        (arrows.to_string(), "Move / extend selection"),
        (format!("{} / {}", k("secondary-up"), k("secondary-down")), "First / last row"),
        ("Page Up / Down".to_string(), "Scroll by a page"),
        (inspect, "Inspect the selected value"),
        (k("secondary-shift-e"), "Export"),
        (k("secondary-shift-m"), "Open in marimo"),
        (k(keys::RELOAD), "Reload"),
        (format!("{} / {} / {}", k("secondary-="), k("secondary--"), k("secondary-0")), "Zoom in / out / reset"),
        (k("secondary-w"), "Close tab"),
    ]
}
