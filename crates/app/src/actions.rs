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

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("cmd-,", OpenSettings, None),
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-n", NewWindow, None),
        KeyBinding::new("cmd-o", OpenFile, None),
        KeyBinding::new("cmd-shift-o", OpenS3, None),
        KeyBinding::new("cmd-l", OpenUrl, None),
        KeyBinding::new("cmd-alt-n", NewSqlConsole, None),
        KeyBinding::new("cmd-w", CloseTab, None),
        KeyBinding::new("ctrl-tab", NextTab, None),
        KeyBinding::new("ctrl-shift-tab", PreviousTab, None),
        KeyBinding::new("cmd-shift-]", NextTab, None),
        KeyBinding::new("cmd-shift-[", PreviousTab, None),
        KeyBinding::new("cmd-r", Reload, None),
        KeyBinding::new("cmd-shift-e", Export, None),
        KeyBinding::new("cmd-f", Find, None),
        KeyBinding::new("cmd-g", GoToRow, None),
        KeyBinding::new("cmd-shift-f", AddFilter, None),
        KeyBinding::new("cmd-shift-k", ClearFilters, None),
        KeyBinding::new("cmd-1", ShowData, None),
        KeyBinding::new("cmd-2", ShowColumns, None),
        KeyBinding::new("cmd-3", ShowMetadata, None),
        KeyBinding::new("cmd-4", ShowSql, None),
        KeyBinding::new("cmd-alt-i", ToggleInspector, None),
        KeyBinding::new("cmd-=", ZoomIn, None),
        KeyBinding::new("cmd-+", ZoomIn, None),
        KeyBinding::new("cmd--", ZoomOut, None),
        KeyBinding::new("cmd-0", ResetZoom, None),
        KeyBinding::new("cmd-/", ShowShortcuts, None),
        // Run SQL from inside the editor; registered after gpui_kit::init so it wins.
        KeyBinding::new("cmd-enter", RunQuery, Some("SqlEditor > Input")),
        KeyBinding::new("cmd-enter", RunQuery, Some("SqlEditor")),
    ]);
}

/// Build the macOS menu bar. Call again when recents change.
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
    cx.set_menus([
        Menu::new("Parquetry").items([
            MenuItem::action("About Parquetry", About),
            MenuItem::action("Check for Updates…", CheckForUpdates).disabled(!crate::updater::is_available()),
            MenuItem::separator(),
            MenuItem::action("Settings…", OpenSettings),
            MenuItem::separator(),
            MenuItem::action("Quit Parquetry", Quit),
        ]),
        Menu::new("File").items([
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
            MenuItem::action("Compare…", Compare),
            MenuItem::action("Reload", Reload),
            MenuItem::separator(),
            MenuItem::action("Close Tab", CloseTab),
        ]),
        Menu::new("Edit").items([
            MenuItem::os_action("Undo", Undo, OsAction::Undo),
            MenuItem::os_action("Redo", Redo, OsAction::Redo),
            MenuItem::separator(),
            MenuItem::os_action("Cut", Cut, OsAction::Cut),
            MenuItem::os_action("Copy", Copy, OsAction::Copy),
            MenuItem::action("Copy with Headers", parquetry_grid::CopyWithHeaders),
            MenuItem::os_action("Paste", Paste, OsAction::Paste),
            MenuItem::os_action("Select All", SelectAll, OsAction::SelectAll),
            MenuItem::separator(),
            MenuItem::action("Find…", Find),
            MenuItem::action("Go to Row…", GoToRow),
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
            MenuItem::separator(),
            MenuItem::action("Zoom In", ZoomIn),
            MenuItem::action("Zoom Out", ZoomOut),
            MenuItem::action("Actual Size", ResetZoom),
            MenuItem::separator(),
            MenuItem::submenu(Menu::new("Appearance").items([
                MenuItem::action("Match System", UseSystemAppearance),
                MenuItem::action("Light", UseLightAppearance),
                MenuItem::action("Dark", UseDarkAppearance),
            ])),
        ]),
        Menu::new("Window").items([
            MenuItem::action("Show Next Tab", NextTab),
            MenuItem::action("Show Previous Tab", PreviousTab),
        ]),
        Menu::new("Help").items([
            MenuItem::action("Keyboard Shortcuts", ShowShortcuts),
            MenuItem::action("Parquetry Help", ShowHelp),
        ]),
    ]);
}

/// The shortcut list shown in the help dialog.
pub const SHORTCUTS: &[(&str, &str)] = &[
    ("⌘O", "Open file"),
    ("⇧⌘O", "Open S3 location"),
    ("⌘L", "Open URL or path"),
    ("⌘F", "Search all columns"),
    ("⇧⌘F", "Add filter"),
    ("⇧⌘K", "Clear filters"),
    ("⌘G", "Go to row"),
    ("⌘1 – ⌘4", "Data, Columns, Metadata, SQL"),
    ("⌥⌘I", "Toggle inspector"),
    ("⌘↵", "Run SQL"),
    ("⌘C / ⇧⌘C", "Copy selection / with headers"),
    ("⌘A", "Select all cells"),
    ("Arrows, ⇧ Arrows", "Move / extend selection"),
    ("⌘↑ / ⌘↓", "First / last row"),
    ("Page Up / Down", "Scroll by a page"),
    ("↵ or Space", "Inspect the selected value"),
    ("⇧⌘E", "Export"),
    ("⌘R", "Reload"),
    ("⌘= / ⌘- / ⌘0", "Zoom in / out / reset"),
    ("⌘W", "Close tab"),
];
