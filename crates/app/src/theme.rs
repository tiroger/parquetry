//! Appearance: light/dark (or following the system) and interface size.

use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::*;

use crate::app_state::AppState;
use crate::settings::Appearance;

/// Apply the saved appearance and font size to every window.
pub fn apply(cx: &mut App) {
    let settings = AppState::settings(cx).clone();
    let mode = match settings.appearance {
        Appearance::Light => ThemeMode::Light,
        Appearance::Dark => ThemeMode::Dark,
        Appearance::System => match cx.window_appearance() {
            WindowAppearance::Dark | WindowAppearance::VibrantDark => ThemeMode::Dark,
            _ => ThemeMode::Light,
        },
    };
    Theme::change(mode, None, cx);
    Theme::update(cx, |theme| {
        theme.font_size = px(settings.font_size);
        theme.mono_font_family = "Menlo".into();
        theme.mono_font_size = px((settings.font_size * 0.87).round());
    });
    cx.refresh_windows();
}

/// Follow system appearance changes while "Match System" is selected.
pub fn observe_system_appearance<T: 'static>(window: &mut Window, cx: &mut Context<T>) -> Subscription {
    cx.observe_window_appearance(window, |_, _, cx| {
        if AppState::settings(cx).appearance == Appearance::System {
            apply(cx);
        }
    })
}
