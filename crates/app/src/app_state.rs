//! Application-wide state: the engine and persisted settings.

use gpui_kit::*;
use parquetry_engine::Engine;

use crate::settings::Settings;

pub struct AppState {
    pub engine: Engine,
    pub settings: Settings,
}

impl Global for AppState {}

impl AppState {
    pub fn engine(cx: &App) -> Engine {
        cx.global::<AppState>().engine.clone()
    }

    pub fn settings(cx: &App) -> &Settings {
        &cx.global::<AppState>().settings
    }

    /// Change settings, persist them, and apply engine/appearance changes.
    pub fn update_settings(cx: &mut App, change: impl FnOnce(&mut Settings)) {
        let (engine_changed, appearance_changed) = cx.update_global::<AppState, _>(|state, _| {
            let before = state.settings.clone();
            change(&mut state.settings);
            state.settings.save();
            let engine_changed = before.engine != state.settings.engine;
            if engine_changed
                && let Err(error) = state.engine.apply_settings(&state.settings.engine) {
                    log::warn!("couldn't apply engine settings: {error}");
                }
            (
                engine_changed,
                before.appearance != state.settings.appearance
                    || before.dark_theme != state.settings.dark_theme
                    || before.font_size != state.settings.font_size,
            )
        });
        if appearance_changed {
            crate::theme::apply(cx);
        }
        let _ = engine_changed;
        cx.refresh_windows();
    }
}
