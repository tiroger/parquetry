//! Appearance: light or one of two dark palettes (or following the system), and
//! interface size. The palettes live in `themes/parquetry.json`.

use std::rc::Rc;
use std::sync::OnceLock;

use gpui_kit::component::{Theme, ThemeConfig, ThemeMode, ThemeSet};
use gpui_kit::*;

use crate::app_state::AppState;
use crate::settings::Appearance;

const THEMES: &str = include_str!("../themes/parquetry.json");

fn themes() -> &'static ThemeSet {
    static SET: OnceLock<ThemeSet> = OnceLock::new();
    SET.get_or_init(|| serde_json::from_str(THEMES).expect("themes/parquetry.json is valid"))
}

fn config(name: &str) -> Rc<ThemeConfig> {
    let theme = themes()
        .themes
        .iter()
        .find(|t| t.name.as_ref() == name)
        .unwrap_or_else(|| panic!("theme {name} missing from themes/parquetry.json"));
    Rc::new(theme.clone())
}

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
    let (light, dark) = (config("Parquetry Light"), config(settings.dark_theme.theme_name()));
    if !cx.has_global::<Theme>() {
        Theme::change(mode, None, cx);
    }
    Theme::update(cx, |theme| {
        theme.light_theme = light;
        theme.dark_theme = dark;
    });
    // `change` reloads the mode's theme even when the mode is unchanged.
    Theme::change(mode, None, cx);
    Theme::update(cx, |theme| {
        theme.font_size = px(settings.font_size);
        // Elsewhere the platform default (Consolas, DejaVu Sans Mono) is right.
        if cfg!(target_os = "macos") {
            theme.mono_font_family = "Menlo".into();
        }
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

#[cfg(test)]
mod tests {
    use super::{config, themes};

    #[test]
    fn bundled_themes_parse() {
        let names: Vec<&str> = themes().themes.iter().map(|t| t.name.as_ref()).collect();
        assert_eq!(names, ["Parquetry Light", "Navy & Oak", "Slate"]);
        for name in names {
            assert!(!config(name).colors.background.is_none(), "{name} sets a background");
        }
    }
}
