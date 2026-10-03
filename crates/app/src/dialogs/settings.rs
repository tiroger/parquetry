//! Settings: appearance, S3 access and performance.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogFooter};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::select::{Select, SelectState};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme as _, IndexPath, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::component::Disableable as _;
use gpui_kit::*;

use crate::app_state::AppState;
use crate::settings::{Appearance, DarkTheme, NotebookLibrary, NotebookTarget, DEFAULT_FONT_SIZE, MAX_FONT_SIZE, MIN_FONT_SIZE, Settings};

/// Profile names from `~/.aws/config` and `~/.aws/credentials`.
pub fn aws_profiles() -> Vec<String> {
    let mut profiles = Vec::new();
    let home = dirs::home_dir().unwrap_or_default();
    let config = std::env::var("AWS_CONFIG_FILE").map(Into::into).unwrap_or_else(|_| home.join(".aws/config"));
    let credentials = std::env::var("AWS_SHARED_CREDENTIALS_FILE")
        .map(Into::into)
        .unwrap_or_else(|_| home.join(".aws/credentials"));
    for (path, prefixed) in [(config, true), (credentials, false)] {
        let Ok(text) = std::fs::read_to_string(path) else { continue };
        profiles.extend(parse_profiles(&text, prefixed));
    }
    profiles.sort();
    profiles.dedup();
    profiles
}

fn parse_profiles(text: &str, prefixed: bool) -> Vec<String> {
    text.lines()
        .filter_map(|line| {
            let section = line.trim().strip_prefix('[')?.strip_suffix(']')?.trim();
            if section == "default" {
                return Some("default".to_string());
            }
            if prefixed {
                section.strip_prefix("profile ").map(|p| p.trim().to_string())
            } else if section.starts_with("sso-session") {
                None
            } else {
                Some(section.to_string())
            }
        })
        .collect()
}

pub struct SettingsForm {
    appearance: Entity<SelectState<Vec<SharedString>>>,
    dark_theme: Entity<SelectState<Vec<SharedString>>>,
    font_size: f32,
    show_summaries: bool,
    reopen_last_session: bool,
    /// Sparkle's automatic-check preference; `None` in builds without updates.
    auto_update: Option<bool>,
    profile: Entity<SelectState<Vec<SharedString>>>,
    profiles: Vec<String>,
    region: Entity<InputState>,
    endpoint: Entity<InputState>,
    notebook_library: Entity<SelectState<Vec<SharedString>>>,
    notebook_target: Entity<SelectState<Vec<SharedString>>>,
    notebooks_dir: Entity<InputState>,
    path_style: bool,
    memory: Entity<InputState>,
    result_limit: Entity<InputState>,
    sample_threshold: Entity<InputState>,
}

impl SettingsForm {
    fn new(settings: &Settings, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let appearance_ix = Appearance::all().iter().position(|a| *a == settings.appearance).unwrap_or(0);
        let dark_ix = DarkTheme::all().iter().position(|t| *t == settings.dark_theme).unwrap_or(0);
        let mut profiles = vec!["Default credential chain".to_string()];
        profiles.extend(aws_profiles());
        let profile_ix = settings
            .engine
            .s3
            .profile
            .as_ref()
            .and_then(|p| profiles.iter().position(|x| x == p))
            .unwrap_or(0);
        let s3 = &settings.engine.s3;
        let input = |value: String, placeholder: &str, window: &mut Window, cx: &mut Context<Self>| {
            let placeholder = placeholder.to_string();
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder).default_value(value))
        };
        Self {
            appearance: cx.new(|cx| {
                SelectState::new(
                    Appearance::all().iter().map(|a| SharedString::from(a.label())).collect::<Vec<_>>(),
                    Some(IndexPath::new(appearance_ix)),
                    window,
                    cx,
                )
            }),
            dark_theme: cx.new(|cx| {
                SelectState::new(
                    DarkTheme::all().iter().map(|t| SharedString::from(t.label())).collect::<Vec<_>>(),
                    Some(IndexPath::new(dark_ix)),
                    window,
                    cx,
                )
            }),
            font_size: settings.font_size,
            show_summaries: settings.show_summaries,
            reopen_last_session: settings.reopen_last_session,
            auto_update: crate::updater::automatically_checks(),
            profile: cx.new(|cx| {
                SelectState::new(
                    profiles.iter().map(|p| SharedString::from(p.clone())).collect::<Vec<_>>(),
                    Some(IndexPath::new(profile_ix)),
                    window,
                    cx,
                )
            }),
            profiles,
            region: input(s3.region.clone().unwrap_or_default(), "Auto-detected per bucket (fallback us-east-1)", window, cx),
            endpoint: input(s3.endpoint.clone().unwrap_or_default(), "AWS (leave empty); e.g. http://localhost:9000 for MinIO", window, cx),
            notebook_library: cx.new(|cx| {
                let ix = NotebookLibrary::all().iter().position(|l| *l == settings.notebook_library).unwrap_or(0);
                SelectState::new(
                    NotebookLibrary::all().iter().map(|l| SharedString::from(l.label())).collect::<Vec<_>>(),
                    Some(IndexPath::new(ix)),
                    window,
                    cx,
                )
            }),
            notebook_target: cx.new(|cx| {
                let ix = NotebookTarget::all().iter().position(|t| *t == settings.notebook_target).unwrap_or(0);
                SelectState::new(
                    NotebookTarget::all().iter().map(|t| SharedString::from(t.label())).collect::<Vec<_>>(),
                    Some(IndexPath::new(ix)),
                    window,
                    cx,
                )
            }),
            notebooks_dir: input(
                settings.notebooks_dir.clone().unwrap_or_default(),
                &crate::format::display_path(&crate::notebook::default_notebooks_dir().to_string_lossy()),
                window,
                cx,
            ),
            path_style: s3.path_style,
            memory: input(settings.engine.memory_limit.clone().unwrap_or_default(), "Automatic (80% of RAM), e.g. 16GB", window, cx),
            result_limit: input(settings.engine.result_limit_rows.to_string(), "10000000", window, cx),
            sample_threshold: input(settings.engine.sample_threshold_rows.to_string(), "100000000", window, cx),
        }
    }

    fn apply(&self, settings: &mut Settings, cx: &App) -> Result<(), String> {
        let appearance_ix = self.appearance.read(cx).selected_index(cx).map(|i| i.row).unwrap_or(0);
        settings.appearance = Appearance::all()[appearance_ix.min(2)];
        let dark_ix = self.dark_theme.read(cx).selected_index(cx).map(|i| i.row).unwrap_or(0);
        settings.dark_theme = DarkTheme::all()[dark_ix.min(DarkTheme::all().len() - 1)];
        settings.font_size = self.font_size;
        settings.show_summaries = self.show_summaries;
        settings.reopen_last_session = self.reopen_last_session;
        if let Some(enabled) = self.auto_update {
            // Stored by Sparkle itself, not in settings.json.
            crate::updater::set_automatically_checks(enabled);
        }
        let profile_ix = self.profile.read(cx).selected_index(cx).map(|i| i.row).unwrap_or(0);
        settings.engine.s3.profile = if profile_ix == 0 { None } else { self.profiles.get(profile_ix).cloned() };
        let text = |input: &Entity<InputState>| -> Option<String> {
            let value = input.read(cx).value().trim().to_string();
            (!value.is_empty()).then_some(value)
        };
        settings.engine.s3.region = text(&self.region);
        settings.engine.s3.endpoint = text(&self.endpoint);
        let library_ix = self.notebook_library.read(cx).selected_index(cx).map(|i| i.row).unwrap_or(0);
        settings.notebook_library = NotebookLibrary::all()[library_ix.min(NotebookLibrary::all().len() - 1)];
        settings.notebooks_dir = text(&self.notebooks_dir);
        let target_ix = self.notebook_target.read(cx).selected_index(cx).map(|i| i.row).unwrap_or(0);
        settings.notebook_target = NotebookTarget::all()[target_ix.min(NotebookTarget::all().len() - 1)];
        settings.engine.s3.path_style = self.path_style;
        settings.engine.memory_limit = text(&self.memory);
        let number = |input: &Entity<InputState>, name: &str| -> Result<Option<u64>, String> {
            match text(input) {
                None => Ok(None),
                Some(v) => crate::dialogs::goto_row::parse_row(&v)
                    .filter(|n| *n > 0)
                    .map(Some)
                    .ok_or_else(|| format!("{name} must be a positive number")),
            }
        };
        if let Some(limit) = number(&self.result_limit, "The result limit")? {
            settings.engine.result_limit_rows = limit;
        }
        if let Some(threshold) = number(&self.sample_threshold, "The sampling threshold")? {
            settings.engine.sample_threshold_rows = threshold;
        }
        if let Some(memory) = &settings.engine.memory_limit {
            let ok = memory
                .trim_end_matches(|c: char| c.is_ascii_alphabetic() || c.is_whitespace())
                .trim()
                .parse::<f64>()
                .is_ok();
            if !ok {
                return Err("Memory limit should look like 8GB or 512MB".into());
            }
        }
        Ok(())
    }
}

fn section(title: &str, cx: &App) -> Div {
    v_flex()
        .gap_2()
        .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).text_color(cx.theme().foreground).child(title.to_string()))
}

fn row(label: &str, control: impl IntoElement, cx: &App) -> Div {
    h_flex()
        .gap_3()
        .child(div().w(rems(9.)).flex_shrink_0().text_sm().text_color(cx.theme().muted_foreground).child(label.to_string()))
        .child(div().flex_1().child(control))
}

impl Render for SettingsForm {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let font_size = self.font_size;
        // Scroll when the window is too short for the whole form.
        let max_height = (window.viewport_size().height * 0.62).max(px(240.));
        v_flex()
            .id("settings-form")
            .max_h(max_height)
            .overflow_y_scroll()
            .pr_2()
            .gap_5()
            .child(
                section("Appearance", cx)
                    .child(row("Theme", Select::new(&self.appearance).small(), cx))
                    .child(row("Dark palette", Select::new(&self.dark_theme).small(), cx))
                    .child(row(
                        "Interface size",
                        h_flex()
                            .gap_2()
                            .child(Button::new("font-smaller").label("A−").small().outline().disabled(font_size <= MIN_FONT_SIZE).on_click(cx.listener(|this, _, _, cx| {
                                this.font_size = (this.font_size - 1.0).max(MIN_FONT_SIZE);
                                cx.notify();
                            })))
                            .child(div().w(rems(3.)).text_center().text_sm().child(format!("{font_size:.0} pt")))
                            .child(Button::new("font-larger").label("A+").small().outline().disabled(font_size >= MAX_FONT_SIZE).on_click(cx.listener(|this, _, _, cx| {
                                this.font_size = (this.font_size + 1.0).min(MAX_FONT_SIZE);
                                cx.notify();
                            })))
                            .child(Button::new("font-reset").label("Reset").small().ghost().on_click(cx.listener(|this, _, _, cx| {
                                this.font_size = DEFAULT_FONT_SIZE;
                                cx.notify();
                            }))),
                        cx,
                    ))
                    .child(row(
                        "Summaries",
                        Switch::new("show-summaries")
                            .label("Show column summaries in headers")
                            .checked(self.show_summaries)
                            .on_click(cx.listener(|this, on: &bool, _, cx| {
                                this.show_summaries = *on;
                                cx.notify();
                            })),
                        cx,
                    ))
                    .child(row(
                        "At launch",
                        Switch::new("reopen-last-session")
                            .label("Reopen windows and tabs from last time")
                            .checked(self.reopen_last_session)
                            .on_click(cx.listener(|this, on: &bool, _, cx| {
                                this.reopen_last_session = *on;
                                cx.notify();
                            })),
                        cx,
                    ))
                    .children(self.auto_update.map(|enabled| {
                        row(
                            "Updates",
                            h_flex()
                                .gap_3()
                                .child(
                                    Switch::new("auto-update")
                                        .label("Check for updates automatically")
                                        .checked(enabled)
                                        .on_click(cx.listener(|this, on: &bool, _, cx| {
                                            this.auto_update = Some(*on);
                                            cx.notify();
                                        })),
                                )
                                .child(
                                    Button::new("check-now")
                                        .label("Check Now")
                                        .small()
                                        .outline()
                                        .on_click(|_, _, _| crate::updater::check_for_updates()),
                                ),
                            cx,
                        )
                    })),
            )
            .child(
                section("Amazon S3", cx)
                    .child(row("Credentials", Select::new(&self.profile).small(), cx))
                    .child(row("Region", Input::new(&self.region).small(), cx))
                    .child(row("Endpoint", Input::new(&self.endpoint).small(), cx))
                    .child(row(
                        "Options",
                        v_flex()
                            .gap_2()
                            .child(Switch::new("path-style").label("Path-style addressing (MinIO, R2, …)").checked(self.path_style).on_click(cx.listener(|this, on: &bool, _, cx| {
                                this.path_style = *on;
                                cx.notify();
                            }))),
                        cx,
                    ))
                    .child(div().text_xs().text_color(cx.theme().muted_foreground).child(
                        "Profiles come from ~/.aws/config. For SSO profiles, run `aws sso login --profile NAME` first. Without any credentials, public buckets still open.",
                    )),
            )
            .child(
                section("Notebooks", cx)
                    .child(row("Open in marimo with", Select::new(&self.notebook_library).small(), cx))
                    .child(row("Show notebooks in", Select::new(&self.notebook_target).small(), cx))
                    .child(row("Save notebooks in", Input::new(&self.notebooks_dir).small(), cx))
                    .child(div().text_xs().text_color(cx.theme().muted_foreground).child(
                        "Notebooks open with uv (docs.astral.sh/uv), which installs marimo and the libraries they need on first use.",
                    )),
            )
            .child(
                section("Performance", cx)
                    .child(row("Memory limit", Input::new(&self.memory).small(), cx))
                    .child(row("Exact summaries up to", Input::new(&self.sample_threshold).small(), cx))
                    .child(row("SQL result limit", Input::new(&self.result_limit).small(), cx))
                    .child(div().text_xs().text_color(cx.theme().muted_foreground).child(
                        "Local datasets with more rows than the summary threshold get sampled summaries (remote ones above 2M rows). Queries spill to disk beyond the memory limit.",
                    )),
            )
    }
}

pub fn open(window: &mut Window, cx: &mut App) {
    let settings = AppState::settings(cx).clone();
    let form = cx.new(|cx| SettingsForm::new(&settings, window, cx));
    window.open_dialog(cx, move |dialog, _, _| {
        let form_ok = form.clone();
        dialog
            .title("Settings")
            .width(px(600.))
            .child(form.clone())
            .footer(
                DialogFooter::new()
                    .child(DialogClose::new().child(Button::new("cancel").label("Cancel").outline()))
                    .child(DialogAction::new().child(Button::new("save").label("Save").primary())),
            )
            .on_ok(move |_, window, cx| {
                let mut next = AppState::settings(cx).clone();
                match form_ok.read(cx).apply(&mut next, cx) {
                    Ok(()) => {
                        AppState::update_settings(cx, |s| *s = next);
                        true
                    }
                    Err(message) => {
                        window.push_notification(
                            gpui_kit::component::notification::Notification::error(message).title("Check your settings"),
                            cx,
                        );
                        false
                    }
                }
            })
    });
}

#[cfg(test)]
mod tests {
    use super::parse_profiles;

    #[test]
    fn profiles() {
        let config = "[default]\nregion=us-east-1\n[profile dev]\nx=1\n[sso-session corp]\n[profile  prod ]\n";
        assert_eq!(parse_profiles(config, true), vec!["default", "dev", "prod"]);
        let creds = "[default]\n[ci]\n";
        assert_eq!(parse_profiles(creds, false), vec!["default", "ci"]);
    }
}
