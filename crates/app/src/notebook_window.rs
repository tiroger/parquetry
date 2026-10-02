//! The notebook window: a marimo notebook in its own Parquetry window, shown in
//! the system web view (WKWebView on macOS, WebView2 on Windows).
//!
//! Parquetry runs marimo headless for the notebook and stops it when the window
//! closes (or when Parquetry quits). The web view is a native view laid over the
//! window's content area, so this window draws nothing of its own beneath it:
//! GPUI popups would end up behind it. Links leaving marimo open in the browser,
//! and downloads go to the Downloads folder.

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::Instant;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, Sizable as _, TitleBar, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::actions::CloseTab;
use crate::notebook::{MarimoServer, open_externally};

enum State {
    Starting { since: Instant },
    Ready { url: String },
    Failed(SharedString),
}

pub struct NotebookWindow {
    title: SharedString,
    path: PathBuf,
    state: State,
    server: Option<MarimoServer>,
    webview: Rc<RefCell<Option<wry::WebView>>>,
    /// The content area, as last painted (the web view follows it).
    content: Rc<Cell<Option<Bounds<Pixels>>>>,
    focus_handle: FocusHandle,
    _tasks: Vec<Task<()>>,
}

impl Focusable for NotebookWindow {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

/// Open `notebook` (a saved marimo notebook) in a new window.
pub fn open(notebook: PathBuf, cx: &mut App) {
    let title: SharedString = notebook
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Notebook".into())
        .into();
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(None, size(px(1280.), px(860.)), cx))),
        window_min_size: Some(size(px(640.), px(420.))),
        app_id: Some(crate::variant::APP_ID.into()),
        ..gpui_kit::component::TitleBar::window_options()
    };
    let result = gpui_kit::open_window(options, cx, move |window, cx| {
        let view = cx.new(|cx| NotebookWindow::new(title, notebook, window, cx));
        let focus = view.read(cx).focus_handle.clone();
        window.focus(&focus, cx);
        view
    });
    if let Err(error) = result {
        log::error!("couldn’t open the notebook window: {error:#}");
    }
}

impl NotebookWindow {
    fn new(title: SharedString, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            title,
            path,
            state: State::Starting { since: Instant::now() },
            server: None,
            webview: Rc::default(),
            content: Rc::default(),
            focus_handle: cx.focus_handle(),
            _tasks: Vec::new(),
        };
        this.start(window, cx);
        this
    }

    fn start(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let log_dir = crate::settings::Settings::directory();
        match MarimoServer::start(&self.path, &log_dir) {
            Ok((server, ready)) => {
                self.server = Some(server);
                self._tasks.push(cx.spawn_in(window, async move |this, cx| {
                    let result = ready.await;
                    let _ = this.update_in(cx, |this, window, cx| {
                        match result {
                            Ok(Ok(url)) => this.show(url, window, cx),
                            Ok(Err(error)) => this.state = State::Failed(format!("{error:#}").into()),
                            Err(_) => this.state = State::Failed("marimo stopped unexpectedly.".into()),
                        }
                        cx.notify();
                    });
                }));
                // Keep the elapsed time ticking while marimo starts.
                self._tasks.push(cx.spawn(async move |this, cx| {
                    loop {
                        cx.background_executor().timer(std::time::Duration::from_secs(1)).await;
                        let starting = this
                            .update(cx, |this, cx| {
                                cx.notify();
                                matches!(this.state, State::Starting { .. })
                            })
                            .unwrap_or(false);
                        if !starting {
                            break;
                        }
                    }
                }));
            }
            Err(error) => self.state = State::Failed(format!("{error:#}").into()),
        }
    }

    /// marimo is serving: put the web view over the content area.
    fn show(&mut self, url: String, window: &mut Window, _cx: &mut Context<Self>) {
        let origin = origin_of(&url);
        let builder = wry::WebViewBuilder::new()
            .with_url(url.clone())
            .with_bounds(rect(self.content.get().unwrap_or_default()))
            .with_devtools(cfg!(debug_assertions))
            .with_accept_first_mouse(true)
            // Stay on marimo; anything else opens in the browser.
            .with_navigation_handler(move |target: String| {
                let local = same_origin(&target, &origin)
                    || ["about:", "blob:", "data:"].iter().any(|scheme| target.starts_with(scheme));
                if !local {
                    open_externally(&target);
                }
                local
            })
            .with_new_window_req_handler(|target, _| {
                open_externally(&target);
                wry::NewWindowResponse::Deny
            })
            .with_download_started_handler(|_, destination| {
                let name = destination
                    .file_name()
                    .map(|n| n.to_os_string())
                    .unwrap_or_else(|| "download".into());
                if let Some(downloads) = dirs::download_dir() {
                    *destination = downloads.join(name);
                }
                true
            });
        match builder.build_as_child(&*window) {
            Ok(webview) => {
                *self.webview.borrow_mut() = Some(webview);
                self.state = State::Ready { url };
            }
            Err(error) => self.state = State::Failed(format!("Couldn’t create the web view: {error}").into()),
        }
    }

    fn open_in_browser(&mut self, cx: &mut Context<Self>) {
        if let State::Ready { url } = &self.state {
            open_externally(url);
        }
        cx.notify();
    }

    fn render_title_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let ready = matches!(self.state, State::Ready { .. });
        let path = self.path.clone();
        TitleBar::new()
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_2()
                    .child(Icon::new(gpui_kit::assets::IconName::NotebookPen).text_color(theme.primary))
                    .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).truncate().child(self.title.clone()))
                    .child(div().text_xs().text_color(theme.muted_foreground).child("marimo")),
            )
            .child(
                h_flex()
                    .pr_2()
                    .gap_1()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        Button::new("notebook-reveal")
                            .label("Show File")
                            .xsmall()
                            .ghost()
                            .on_click(move |_, _, cx| cx.reveal_path(&path)),
                    )
                    .child(
                        Button::new("notebook-browser")
                            .label("Open in Browser")
                            .xsmall()
                            .ghost()
                            .disabled(!ready)
                            .on_click(cx.listener(|this, _, _, cx| this.open_in_browser(cx))),
                    ),
            )
    }

    fn render_status(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        match &self.state {
            State::Starting { since } => v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_3()
                .child(Spinner::new().large())
                .child(div().text_base().child("Starting marimo…"))
                .child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(format!("{} s · the first time, uv installs marimo and the notebook’s libraries", since.elapsed().as_secs())),
                )
                .into_any_element(),
            State::Failed(message) => v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_2()
                .p_8()
                .child(div().text_base().text_color(theme.danger).child("Couldn’t open the notebook"))
                .child(
                    div()
                        .max_w(rems(48.))
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .font_family(theme.mono_font_family.clone())
                        .whitespace_normal()
                        .child(message.clone()),
                )
                .into_any_element(),
            State::Ready { .. } => div().into_any_element(),
        }
    }
}

impl Render for NotebookWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        window.set_window_title(&format!("{} — {}", self.title, crate::variant::APP_NAME));
        let theme = cx.theme().clone();
        let (content, webview) = (self.content.clone(), self.webview.clone());
        let ready = matches!(self.state, State::Ready { .. });
        v_flex()
            .id("notebook-window")
            .key_context("NotebookWindow")
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            // ⌘W / Ctrl+W closes this window (marimo stops with it).
            .on_action(cx.listener(|_, _: &CloseTab, window, _| window.remove_window()))
            .child(self.render_title_bar(cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .relative()
                    .child(
                        // Tracks the content area; the web view is laid over it.
                        canvas(
                            |bounds, _, _| bounds,
                            move |_, bounds, _, _| {
                                if content.get() != Some(bounds) {
                                    content.set(Some(bounds));
                                    if let Some(webview) = webview.borrow().as_ref() {
                                        let _ = webview.set_bounds(rect(bounds));
                                    }
                                }
                            },
                        )
                        .absolute()
                        .size_full(),
                    )
                    .when(!ready, |this| this.child(self.render_status(cx))),
            )
    }
}

/// GPUI bounds (logical pixels from the window's top left) as a web view rect.
fn rect(bounds: Bounds<Pixels>) -> wry::Rect {
    wry::Rect {
        position: wry::dpi::LogicalPosition::new(f64::from(f32::from(bounds.origin.x)), f64::from(f32::from(bounds.origin.y))).into(),
        size: wry::dpi::LogicalSize::new(f64::from(f32::from(bounds.size.width)), f64::from(f32::from(bounds.size.height))).into(),
    }
}

/// Whether `url` is on `origin` exactly (not merely sharing its prefix).
fn same_origin(url: &str, origin: &str) -> bool {
    url.strip_prefix(origin).is_some_and(|rest| rest.is_empty() || rest.starts_with(['/', '?', '#']))
}

/// `http://localhost:2718?access_token=…` → `http://localhost:2718`.
fn origin_of(url: &str) -> String {
    let after_scheme = url.find("://").map(|i| i + 3).unwrap_or(0);
    let end = url[after_scheme..].find(['/', '?', '#']).map(|i| after_scheme + i).unwrap_or(url.len());
    url[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::{origin_of, same_origin};

    #[test]
    fn origins() {
        assert_eq!(origin_of("http://localhost:2718?access_token=x"), "http://localhost:2718");
        assert_eq!(origin_of("http://127.0.0.1:2719/files/a?b"), "http://127.0.0.1:2719");
        assert!(same_origin("http://localhost:2718/api?x", "http://localhost:2718"));
        assert!(same_origin("http://localhost:2718", "http://localhost:2718"));
        assert!(!same_origin("http://localhost:27180/", "http://localhost:2718"));
        assert!(!same_origin("https://docs.marimo.io/", "http://localhost:2718"));
    }
}
