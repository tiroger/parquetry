//! Reopen where you left off: the open windows and tabs (with their filters, sort,
//! column layout and position), and the column layout of recently viewed files.
//!
//! The session is written to `<config dir>/session.json` every few seconds while it
//! changes, and when the app quits. At launch without files to open, the saved
//! windows come back. Opening a file later reuses its last column layout.

use std::path::PathBuf;
use std::time::Duration;

use gpui_kit::*;
use parquetry_engine::{Format, ViewSpec};
use parquetry_grid::ColumnArrangement;
use serde::{Deserialize, Serialize};

use crate::app_state::AppState;
use crate::settings::Settings;

/// Locations whose column layout is remembered.
const MAX_LAYOUTS: usize = 200;
const SAVE_EVERY: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Session {
    pub windows: Vec<WindowSession>,
    /// Column layouts by location, most recently used first.
    pub layouts: Vec<(String, ColumnArrangement)>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WindowSession {
    pub tabs: Vec<TabSession>,
    pub active: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TabSession {
    Dataset(Box<DatasetSession>),
    Sql { query: String },
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DatasetSession {
    pub location: String,
    pub format: Option<Format>,
    pub view: ViewSpec,
    pub layout: ColumnArrangement,
    /// "data", "columns", "metadata" or "sql".
    pub tab: String,
    pub top_row: u64,
    pub inspector: bool,
}

impl Session {
    pub fn path() -> PathBuf {
        Settings::directory().join("session.json")
    }

    /// The saved session; empty when missing or unreadable.
    pub fn load() -> Self {
        std::fs::read(Self::path())
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    fn save(&self) {
        let path = Self::path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        let Ok(json) = serde_json::to_vec_pretty(self) else { return };
        // Write then rename, so a crash mid-write never leaves a torn file.
        let temp = path.with_extension("json.tmp");
        if std::fs::write(&temp, json).is_ok() {
            let _ = std::fs::rename(&temp, &path);
        }
    }

    pub fn layout_for(&self, location: &str) -> Option<&ColumnArrangement> {
        self.layouts.iter().find(|(l, _)| l == location).map(|(_, layout)| layout)
    }

    /// Remember `layout` for `location` (most recent first).
    fn remember_layout(&mut self, location: &str, layout: ColumnArrangement) {
        self.layouts.retain(|(l, _)| l != location);
        self.layouts.insert(0, (location.to_string(), layout));
        self.layouts.truncate(MAX_LAYOUTS);
    }
}

/// The session as it will be saved: the open windows, front first, and layouts
/// updated from every open dataset.
fn snapshot(previous: &Session, cx: &App) -> Session {
    let mut session = previous.clone();
    let windows: Vec<WindowSession> = crate::workspace::workspaces_front_to_back(cx)
        .into_iter()
        .map(|workspace| workspace.read(cx).session(cx))
        .collect();
    for window in windows.iter().rev() {
        for tab in window.tabs.iter().rev() {
            if let TabSession::Dataset(d) = tab {
                session.remember_layout(&d.location, d.layout.clone());
            }
        }
    }
    // With no window open (the last one closed, or the app is quitting after
    // closing them), keep the windows that were saved before.
    if !windows.is_empty() {
        session.windows = windows;
    }
    session
}

/// Keeps the session file up to date while the app runs.
pub struct SessionKeeper {
    saved: Session,
}

impl Global for SessionKeeper {}

impl SessionKeeper {
    /// Load the saved session and start saving changes periodically and at quit.
    pub fn start(cx: &mut App) -> Session {
        let saved = Session::load();
        cx.set_global(SessionKeeper { saved: saved.clone() });
        cx.spawn(async move |cx: &mut AsyncApp| {
            loop {
                cx.background_executor().timer(SAVE_EVERY).await;
                cx.update(Self::save_if_changed);
            }
        })
        .detach();
        cx.on_app_quit(|cx| {
            Self::save_if_changed(cx);
            async {}
        })
        .detach();
        saved
    }

    pub fn save_if_changed(cx: &mut App) {
        if !cx.has_global::<SessionKeeper>() {
            return;
        }
        let current = snapshot(&cx.global::<SessionKeeper>().saved, cx);
        if current != cx.global::<SessionKeeper>().saved {
            current.save();
            cx.global_mut::<SessionKeeper>().saved = current;
        }
    }

    /// The remembered layout for a location, if any.
    pub fn layout_for(location: &str, cx: &App) -> Option<ColumnArrangement> {
        cx.try_global::<SessionKeeper>()?.saved.layout_for(location).cloned()
    }
}

/// Whether to reopen the last session at launch.
pub fn should_restore(cx: &App) -> bool {
    AppState::settings(cx).reopen_last_session
}

#[cfg(test)]
mod tests {
    use super::{ColumnArrangement, DatasetSession, MAX_LAYOUTS, Session, TabSession, WindowSession};
    use parquetry_engine::{Filter, FilterOp, SortKey, ViewSpec};

    #[test]
    fn sessions_round_trip_and_layouts_stay_bounded() {
        let mut session = Session {
            windows: vec![WindowSession {
                tabs: vec![
                    TabSession::Dataset(Box::new(DatasetSession {
                        location: "/data/a.parquet".into(),
                        view: ViewSpec {
                            filters: vec![Filter::new("x", FilterOp::Greater, "1")],
                            sort: vec![SortKey::desc("x")],
                            ..Default::default()
                        },
                        layout: ColumnArrangement { order: vec!["x".into()], pinned: 1, hidden: vec!["y".into()], widths: vec![("x".into(), 12.0)] },
                        tab: "columns".into(),
                        top_row: 1234,
                        ..Default::default()
                    })),
                    TabSession::Sql { query: "SELECT 1".into() },
                ],
                active: 1,
            }],
            layouts: Vec::new(),
        };
        let json = serde_json::to_string(&session).unwrap();
        assert_eq!(serde_json::from_str::<Session>(&json).unwrap(), session);
        // Unknown or missing fields don't lose the rest.
        assert_eq!(serde_json::from_str::<Session>("{}").unwrap(), Session::default());

        for i in 0..(MAX_LAYOUTS + 10) {
            session.remember_layout(&format!("/f{i}"), ColumnArrangement::default());
        }
        session.remember_layout("/f3", ColumnArrangement { pinned: 2, ..Default::default() });
        assert_eq!(session.layouts.len(), MAX_LAYOUTS);
        assert_eq!(session.layouts[0].0, "/f3");
        assert_eq!(session.layout_for("/f3").unwrap().pinned, 2);
        assert!(session.layout_for("/f0").is_none(), "oldest dropped");
    }
}
