//! Open a URL, path or glob, with a browser for S3 buckets and prefixes.

use std::rc::Rc;
use std::time::Duration;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::select::{Select, SelectEvent, SelectState};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, IndexPath, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::component::Disableable as _;
use gpui_kit::*;
use parquetry_engine::{CredentialIssue, Format, S3Entry, S3Url, SourceSpec, format_from_extension};

use crate::app_state::AppState;
use crate::format;

type OnOpen = Rc<dyn Fn(SourceSpec, &mut Window, &mut App)>;

pub struct LocationBrowser {
    input: Entity<InputState>,
    format: Entity<SelectState<Vec<SharedString>>>,
    credentials: Entity<SelectState<Vec<SharedString>>>,
    credential_choices: Vec<Credential>,
    entries: Vec<S3Entry>,
    listed: Option<String>,
    listing: Option<Task<()>>,
    debounce: Option<Task<()>>,
    error: Option<SharedString>,
    /// The credentials problem behind `error`, shown as a way to fix it.
    issue: Option<CredentialIssue>,
    selected: Option<usize>,
    scroll: UniformListScrollHandle,
    on_open: OnOpen,
    _subscriptions: Vec<Subscription>,
}

#[derive(Clone, PartialEq)]
enum Credential {
    Default,
    Profile(String),
}

impl Credential {
    fn label(&self) -> String {
        match self {
            Credential::Default => "Default AWS credentials".into(),
            Credential::Profile(p) => format!("Profile: {p}"),
        }
    }
}

const FORMAT_CHOICES: [(&str, Option<Format>); 7] = [
    ("Detect format", None),
    ("Parquet", Some(Format::Parquet)),
    ("CSV", Some(Format::Csv)),
    ("JSON", Some(Format::Json)),
    ("Delta Lake", Some(Format::Delta)),
    ("Iceberg", Some(Format::Iceberg)),
    ("Arrow IPC", Some(Format::Arrow)),
];

impl LocationBrowser {
    fn new(initial: String, on_open: OnOpen, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("s3://bucket/prefix/, https://…/file.parquet, ~/data/*.parquet")
                .default_value(initial.clone())
        });
        let format = cx.new(|cx| {
            SelectState::new(
                FORMAT_CHOICES.iter().map(|(l, _)| SharedString::from(*l)).collect::<Vec<_>>(),
                Some(IndexPath::new(0)),
                window,
                cx,
            )
        });
        let mut credential_choices = vec![Credential::Default];
        credential_choices.extend(crate::dialogs::settings::aws_profiles().into_iter().map(Credential::Profile));
        let s3 = AppState::settings(cx).engine.s3.clone();
        let current = s3.profile.clone().filter(|p| !p.is_empty()).map(Credential::Profile).unwrap_or(Credential::Default);
        let current_ix = credential_choices.iter().position(|c| *c == current).unwrap_or(0);
        let credentials = cx.new(|cx| {
            SelectState::new(
                credential_choices.iter().map(|c| SharedString::from(c.label())).collect::<Vec<_>>(),
                Some(IndexPath::new(current_ix)),
                window,
                cx,
            )
        });
        let subscriptions = vec![
            cx.subscribe_in(&input, window, Self::on_input),
            cx.subscribe_in(&credentials, window, |this, state, _: &SelectEvent<Vec<SharedString>>, _, cx| {
                let ix = state.read(cx).selected_index(cx).map(|i| i.row).unwrap_or(0);
                let Some(choice) = this.credential_choices.get(ix).cloned() else { return };
                AppState::update_settings(cx, |s| match &choice {
                    Credential::Default => s.engine.s3.profile = None,
                    Credential::Profile(p) => s.engine.s3.profile = Some(p.clone()),
                });
                let text = this.input.read(cx).value().trim().to_string();
                if is_browsable(&text) {
                    this.list(text, cx);
                }
            }),
        ];
        let mut this = Self {
            input,
            format,
            credentials,
            credential_choices,
            entries: Vec::new(),
            listed: None,
            listing: None,
            debounce: None,
            error: None,
            issue: None,
            selected: None,
            scroll: UniformListScrollHandle::new(),
            on_open,
            _subscriptions: subscriptions,
        };
        if is_browsable(&initial) {
            this.list(initial, cx);
        }
        this
    }

    fn on_input(&mut self, state: &Entity<InputState>, event: &InputEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let text = state.read(cx).value().trim().to_string();
        // Enter reaches the dialog as Confirm and is handled by `confirm`.
        if let InputEvent::Change = event {
            self.error = None;
            self.issue = None;
            if is_browsable(&text) {
                // List after typing pauses.
                self.debounce = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
                    cx.background_executor().timer(Duration::from_millis(400)).await;
                    let _ = this.update(cx, |this, cx| this.list(text, cx));
                }));
            }
            cx.notify();
        }
    }

    fn list(&mut self, url: String, cx: &mut Context<Self>) {
        let engine = AppState::engine(cx);
        let job = engine.s3_list(url.clone());
        self.error = None;
        self.issue = None;
        self.selected = None;
        self.listing = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| {
            let result = job.await;
            let _ = this.update(cx, |this, cx| {
                this.listing = None;
                match result {
                    Ok(entries) => {
                        this.entries = entries;
                        this.listed = Some(url);
                    }
                    Err(error) if error.is_cancelled() => {}
                    Err(error) => {
                        this.entries.clear();
                        this.listed = None;
                        this.issue = crate::credentials::issue(&error.to_string());
                        this.error = Some(error.to_string().into());
                    }
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    /// List the typed location again (after credentials change).
    fn relist(&mut self, cx: &mut Context<Self>) {
        let text = self.input.read(cx).value().trim().to_string();
        if is_browsable(&text) {
            self.list(text, cx);
        }
    }

    /// After a profile was chosen or signing in finished: show the profile in
    /// use and list again.
    fn credentials_changed(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let current = crate::credentials::current_profile(cx).map(Credential::Profile).unwrap_or(Credential::Default);
        if let Some(ix) = self.credential_choices.iter().position(|c| *c == current) {
            self.credentials.update(cx, |s, cx| s.set_selected_index(Some(IndexPath::new(ix)), window, cx));
        }
        self.relist(cx);
    }

    fn navigate(&mut self, url: String, window: &mut Window, cx: &mut Context<Self>) {
        self.input.update(cx, |s, cx| s.set_value(url.clone(), window, cx));
        self.list(url, cx);
    }

    fn up(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.input.read(cx).value().trim().to_string();
        if let Some(parent) = parent_prefix(&text) {
            self.navigate(parent, window, cx);
        }
    }

    fn activate(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(entry) = self.entries.get(ix).cloned() else { return };
        if entry.is_dir {
            self.navigate(entry.url, window, cx);
        } else {
            self.open(entry.url, window, cx);
        }
    }

    fn open_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let text = match self.selected.and_then(|ix| self.entries.get(ix)) {
            Some(entry) => entry.url.clone(),
            None => self.input.read(cx).value().trim().to_string(),
        };
        if text.is_empty() || text == "s3://" {
            return;
        }
        self.open(text, window, cx);
    }

    /// Enter in the dialog: list a typed prefix, enter a selected folder, or open.
    /// Returns whether the dialog should close.
    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let text = self.input.read(cx).value().trim().to_string();
        if is_browsable(&text) && self.listed.as_deref() != Some(text.as_str()) {
            self.list(text, cx);
            return false;
        }
        if let Some(entry) = self.selected.and_then(|ix| self.entries.get(ix)).cloned() {
            if entry.is_dir {
                self.navigate(entry.url, window, cx);
                return false;
            }
            self.open_without_closing(entry.url, window, cx);
            return true;
        }
        if text.is_empty() || text == "s3://" {
            return false;
        }
        self.open_without_closing(text, window, cx);
        true
    }

    fn open(&mut self, location: String, window: &mut Window, cx: &mut Context<Self>) {
        window.close_dialog(cx);
        self.open_without_closing(location, window, cx);
    }

    fn open_without_closing(&mut self, location: String, window: &mut Window, cx: &mut Context<Self>) {
        let ix = self.format.read(cx).selected_index(cx).map(|i| i.row).unwrap_or(0);
        let mut spec = SourceSpec::new(location);
        if let Some(format) = FORMAT_CHOICES.get(ix).and_then(|(_, f)| *f) {
            spec = spec.with_format(format);
        }
        (self.on_open)(spec, window, cx);
    }

    fn render_rows(&mut self, range: std::ops::Range<usize>, _: &mut Window, cx: &mut Context<Self>) -> Vec<AnyElement> {
        let theme = cx.theme().clone();
        range
            .filter_map(|ix| {
                let entry = self.entries.get(ix)?.clone();
                let selected = self.selected == Some(ix);
                let openable = entry.is_dir || format_from_extension(&entry.name).is_some();
                let icon = if entry.is_dir {
                    Icon::new(IconName::Folder)
                } else if openable {
                    Icon::new(Lucide::Sheet)
                } else {
                    Icon::new(IconName::File)
                };
                Some(
                    h_flex()
                        .id(("s3-entry", ix))
                        .h(rems(1.875))
                        .px_2()
                        .gap_2()
                        .rounded(theme.radius)
                        .when(selected, |this| this.bg(theme.accent).text_color(theme.accent_foreground))
                        .when(!selected, |this| this.hover(|s| s.bg(theme.table_hover)))
                        .when(!openable, |this| this.text_color(theme.muted_foreground))
                        .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                            if event.click_count() >= 2 {
                                this.activate(ix, window, cx);
                            } else {
                                this.selected = Some(ix);
                                cx.notify();
                            }
                        }))
                        .child(icon.small())
                        .child(div().flex_1().min_w_0().truncate().text_sm().child(entry.name.clone()))
                        .child(
                            div()
                                .w(rems(6.))
                                .text_xs()
                                .text_right()
                                .text_color(theme.muted_foreground)
                                .child(entry.size.map(format::bytes).unwrap_or_default()),
                        )
                        .child(
                            div()
                                .w(rems(10.))
                                .text_xs()
                                .text_right()
                                .text_color(theme.muted_foreground)
                                .truncate()
                                .child(entry.modified.clone().unwrap_or_default().replace('T', " ").trim_end_matches('Z').to_string()),
                        )
                        .into_any_element(),
                )
            })
            .collect()
    }
}

/// Worth listing: `s3://`, a bucket, or a prefix ending in `/`.
fn is_browsable(text: &str) -> bool {
    match S3Url::parse(text) {
        Some(url) => url.key.is_empty() || url.key.ends_with('/'),
        None => false,
    }
}

fn parent_prefix(text: &str) -> Option<String> {
    let url = S3Url::parse(text)?;
    if url.bucket.is_empty() {
        return None;
    }
    let key = url.key.trim_end_matches('/');
    if key.is_empty() {
        return Some("s3://".into());
    }
    match key.rsplit_once('/') {
        Some((parent, _)) => Some(format!("s3://{}/{parent}/", url.bucket)),
        None => Some(format!("s3://{}/", url.bucket)),
    }
}

impl Render for LocationBrowser {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let text = self.input.read(cx).value().to_string();
        let browsing = S3Url::parse(text.trim()).is_some();
        v_flex()
            .gap_3()
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Button::new("s3-up")
                            .icon(IconName::ArrowUp)
                            .small()
                            .outline()
                            .tooltip("Enclosing folder")
                            .disabled(parent_prefix(text.trim()).is_none())
                            .on_click(cx.listener(|this, _, window, cx| this.up(window, cx))),
                    )
                    .child(div().flex_1().child(Input::new(&self.input).cleanable(true)))
                    .child(div().w(rems(10.)).child(Select::new(&self.format).small())),
            )
            .when(browsing, |this| {
                let count = self.entries.len();
                this.child(
                    v_flex()
                        .h(rems(20.))
                        .p_1()
                        .rounded(theme.radius)
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.table)
                        .map(|this| {
                            if self.listing.is_some() {
                                this.child(h_flex().p_3().gap_2().text_sm().child(Spinner::new().small()).child("Listing…"))
                            } else if let Some(issue) = self.issue {
                                let browser = cx.entity().downgrade();
                                let retry: crate::credentials::Retry = Rc::new(move |window, cx| {
                                    let _ = browser.update(cx, |this, cx| this.credentials_changed(window, cx));
                                });
                                this.child(crate::credentials::panel(issue, retry, None, cx))
                            } else if let Some(error) = &self.error {
                                this.child(div().p_3().text_sm().text_color(theme.danger).whitespace_normal().child(error.clone()))
                            } else if count == 0 && self.listed.is_some() {
                                this.child(div().p_3().text_sm().text_color(theme.muted_foreground).child("This folder is empty"))
                            } else {
                                this.child(
                                    uniform_list("s3-entries", count, cx.processor(Self::render_rows))
                                        .track_scroll(&self.scroll)
                                        .size_full(),
                                )
                            }
                        }),
                )
            })
            .child(
                h_flex()
                    .justify_between()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(if browsing {
                        h_flex()
                            .gap_2()
                            .child("Credentials")
                            .child(div().w(rems(16.)).child(Select::new(&self.credentials).small()))
                            .into_any_element()
                    } else {
                        div()
                            .child("Local paths, globs, folders, http(s):// and s3:// URLs are supported.")
                            .into_any_element()
                    })
                    .child(
                        Button::new("open-location")
                            .primary()
                            .small()
                            .label(if self.selected.and_then(|ix| self.entries.get(ix)).is_some_and(|e| e.is_dir) {
                                "Open Folder"
                            } else {
                                "Open"
                            })
                            .disabled(text.trim().is_empty() || text.trim() == "s3://")
                            .on_click(cx.listener(|this, _, window, cx| this.open_text(window, cx))),
                    ),
            )
    }
}

pub fn open(title: &str, initial: String, on_open: impl Fn(SourceSpec, &mut Window, &mut App) + 'static, window: &mut Window, cx: &mut App) {
    let on_open: OnOpen = Rc::new(on_open);
    let browser = cx.new(|cx| LocationBrowser::new(initial, on_open, window, cx));
    let input = browser.read(cx).input.clone();
    let title: SharedString = title.to_string().into();
    window.open_dialog(cx, move |dialog, _, _| {
        let confirm = browser.clone();
        dialog
            .title(title.clone())
            .width(px(720.))
            .child(browser.clone())
            .on_ok(move |_, window, cx| confirm.update(cx, |b, cx| b.confirm(window, cx)))
    });
    crate::dialogs::focus_after_open(input, window);
}

#[cfg(test)]
mod tests {
    use super::{is_browsable, parent_prefix};

    #[test]
    fn s3_navigation() {
        assert!(is_browsable("s3://"));
        assert!(is_browsable("s3://bucket"));
        assert!(is_browsable("s3://bucket/a/"));
        assert!(!is_browsable("s3://bucket/a/file.parquet"));
        assert!(!is_browsable("/tmp/"));
        assert_eq!(parent_prefix("s3://b/a/c/").as_deref(), Some("s3://b/a/"));
        assert_eq!(parent_prefix("s3://b/a/").as_deref(), Some("s3://b/"));
        assert_eq!(parent_prefix("s3://b/").as_deref(), Some("s3://"));
        assert_eq!(parent_prefix("s3://").as_deref(), None);
        assert_eq!(parent_prefix("/tmp"), None);
    }
}
