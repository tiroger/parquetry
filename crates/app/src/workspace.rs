//! The window: a tab strip in the title bar, and the active document below it.

use std::time::{Duration, Instant};

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{
    ActiveTheme as _, Icon, IconName, Sizable as _, TitleBar, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::component::menu::{AppMenuBar, DropdownMenu as _};
use gpui_kit::*;
use parquetry_engine::{Canceller, CompareOptions, Dataset, SourceSpec};

use crate::actions::*;
use crate::app_state::AppState;
use crate::compare_view::CompareView;
use crate::document::{DatasetDocument, DocumentEvent};
use crate::format;
use crate::sql_panel::{OpenDatasets, SqlEvent, SqlPanel};

enum TabContent {
    Loading {
        spec: SourceSpec,
        started: Instant,
        canceller: Canceller,
        _task: Task<()>,
    },
    Failed {
        spec: SourceSpec,
        error: SharedString,
    },
    Dataset(Entity<DatasetDocument>),
    Sql(Entity<SqlPanel>),
    Compare(Entity<CompareView>),
}

struct WorkspaceTab {
    id: u64,
    content: TabContent,
    _subscription: Option<Subscription>,
}

/// All open workspace windows, most recently created last.
#[derive(Default)]
pub struct Workspaces(pub Vec<(AnyWindowHandle, WeakEntity<Workspace>)>);

impl Global for Workspaces {}

pub struct Workspace {
    tabs: Vec<WorkspaceTab>,
    active: usize,
    next_id: u64,
    focus_handle: FocusHandle,
    ticker: Option<Task<()>>,
    /// The in-window menu bar (Windows and Linux; macOS uses the system menu bar).
    menu_bar: Option<Entity<AppMenuBar>>,
    _subscriptions: Vec<Subscription>,
}

impl Focusable for Workspace {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Workspace {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let subscriptions = vec![crate::theme::observe_system_appearance(window, cx)];
        Self {
            tabs: Vec::new(),
            active: 0,
            next_id: 1,
            focus_handle: cx.focus_handle(),
            ticker: None,
            menu_bar: (!cfg!(target_os = "macos")).then(|| AppMenuBar::new(cx)),
            _subscriptions: subscriptions,
        }
    }

    fn next_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        // Keys in the global dataset registry must be unique across windows.
        (cx_window_salt() << 32) | id
    }

    // ------------------------------------------------------------ opening

    /// Open a location in a new tab (or switch to it if it's already open).
    pub fn open(&mut self, spec: SourceSpec, window: &mut Window, cx: &mut Context<Self>) {
        let existing = self.tabs.iter().position(|t| match &t.content {
            TabContent::Dataset(doc) => doc.read(cx).dataset.source() == Some(&spec),
            TabContent::Loading { spec: s, .. } => *s == spec,
            _ => false,
        });
        if let Some(ix) = existing {
            self.activate(ix, window, cx);
            return;
        }
        let id = self.next_id();
        let content = self.start_loading(id, spec, window, cx);
        self.tabs.push(WorkspaceTab {
            id,
            content,
            _subscription: None,
        });
        self.active = self.tabs.len() - 1;
        self.ensure_ticker(cx);
        cx.notify();
    }

    fn start_loading(&mut self, id: u64, spec: SourceSpec, window: &mut Window, cx: &mut Context<Self>) -> TabContent {
        let engine = AppState::engine(cx);
        let job = Dataset::open(&engine, spec.clone());
        let canceller = job.canceller();
        let task_spec = spec.clone();
        let task = cx.spawn_in(window, async move |this, cx| {
            let result = job.await;
            let _ = this.update_in(cx, |this, window, cx| {
                let Some(ix) = this.tabs.iter().position(|t| t.id == id) else { return };
                match result {
                    Ok(dataset) => {
                        AppState::update_settings(cx, |s| s.add_recent(&task_spec.location, task_spec.format));
                        crate::actions::set_menus(cx);
                        this.install_dataset(ix, dataset, None, window, cx);
                    }
                    Err(error) if error.is_cancelled() => {
                        this.close_tab(ix, window, cx);
                    }
                    Err(error) => {
                        this.tabs[ix].content = TabContent::Failed {
                            spec: task_spec,
                            error: error.to_string().into(),
                        };
                    }
                }
                cx.notify();
            });
        });
        TabContent::Loading {
            spec,
            started: Instant::now(),
            canceller,
            _task: task,
        }
    }

    fn install_dataset(&mut self, ix: usize, dataset: Dataset, title: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.tabs[ix].id;
        OpenDatasets::register(id, &dataset, cx);
        let doc = cx.new(|cx| DatasetDocument::new(dataset, title, window, cx));
        let subscription = cx.subscribe_in(&doc, window, |this, doc, event: &DocumentEvent, window, cx| match event {
            DocumentEvent::OpenDataset { dataset, title } => this.open_dataset(dataset.clone(), title.clone(), window, cx),
            DocumentEvent::Compare => {
                let dataset = doc.read(cx).dataset.clone();
                this.compare(Some(dataset), window, cx);
            }
        });
        self.tabs[ix].content = TabContent::Dataset(doc.clone());
        self.tabs[ix]._subscription = Some(subscription);
        if ix == self.active {
            DatasetDocument::focus_grid(&doc, window, cx);
        }
    }

    /// Open an already-loaded dataset (a query result) in a new tab.
    pub fn open_dataset(&mut self, dataset: Dataset, title: String, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.next_id();
        self.tabs.push(WorkspaceTab {
            id,
            content: TabContent::Failed {
                spec: SourceSpec::new(""),
                error: "".into(),
            },
            _subscription: None,
        });
        let ix = self.tabs.len() - 1;
        self.install_dataset(ix, dataset, Some(title), window, cx);
        self.activate(ix, window, cx);
    }

    pub fn open_sql_console(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.next_id();
        let panel = cx.new(|cx| SqlPanel::new(None, window, cx));
        let subscription = cx.subscribe_in(&panel, window, |this, _, event: &SqlEvent, window, cx| match event {
            SqlEvent::OpenResult { dataset, title } => this.open_dataset(dataset.clone(), title.clone(), window, cx),
        });
        SqlPanel::focus_editor(&panel, window, cx);
        self.tabs.push(WorkspaceTab {
            id,
            content: TabContent::Sql(panel),
            _subscription: Some(subscription),
        });
        self.active = self.tabs.len() - 1;
        cx.notify();
    }

    fn compare(&mut self, preselect: Option<Dataset>, window: &mut Window, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        crate::dialogs::compare::open(
            preselect,
            move |left, right, options: CompareOptions, window, cx| {
                let _ = this.update(cx, |this, cx| {
                    let id = this.next_id();
                    let view = cx.new(|cx| CompareView::new(left, right, options, cx));
                    this.tabs.push(WorkspaceTab {
                        id,
                        content: TabContent::Compare(view),
                        _subscription: None,
                    });
                    this.active = this.tabs.len() - 1;
                    let _ = window;
                    cx.notify();
                });
            },
            window,
            cx,
        );
    }

    fn prompt_open(&mut self, directories: bool, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: !directories,
            directories,
            multiple: true,
            prompt: Some("Open".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let Some(paths) = paths.await.ok().and_then(|r| r.ok()).flatten() else { return };
            let _ = this.update_in(cx, |this, window, cx| {
                for path in paths {
                    this.open(SourceSpec::new(path.to_string_lossy()), window, cx);
                }
            });
        })
        .detach();
    }

    fn open_location_dialog(&mut self, s3: bool, window: &mut Window, cx: &mut Context<Self>) {
        let this = cx.entity().downgrade();
        let last_s3 = AppState::settings(cx)
            .recents
            .iter()
            .find(|r| r.location.starts_with("s3://"))
            .map(|r| {
                // Start in the folder of the last S3 location.
                match r.location.rsplit_once('/') {
                    Some((dir, _)) if dir.len() > "s3:/".len() => format!("{dir}/"),
                    _ => "s3://".to_string(),
                }
            });
        let (title, initial) = if s3 {
            ("Open S3 Location", last_s3.unwrap_or_else(|| "s3://".into()))
        } else {
            ("Open URL or Path", String::new())
        };
        crate::dialogs::open_location::open(
            title,
            initial,
            move |spec, window, cx| {
                let _ = this.update(cx, |this, cx| this.open(spec, window, cx));
            },
            window,
            cx,
        );
    }

    // ------------------------------------------------------------ tabs

    fn activate(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() {
            window.focus(&self.focus_handle, cx);
            return;
        }
        self.active = ix;
        match &self.tabs[ix].content {
            TabContent::Dataset(doc) => {
                let doc = doc.clone();
                DatasetDocument::focus_grid(&doc, window, cx)
            }
            TabContent::Sql(panel) => {
                let panel = panel.clone();
                SqlPanel::focus_editor(&panel, window, cx)
            }
            _ => window.focus(&self.focus_handle, cx),
        }
        cx.notify();
    }

    fn close_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if ix >= self.tabs.len() {
            return;
        }
        let tab = self.tabs.remove(ix);
        if let TabContent::Loading { canceller, .. } = &tab.content {
            canceller.cancel();
        }
        OpenDatasets::unregister(tab.id, cx);
        if self.active >= self.tabs.len() {
            self.active = self.tabs.len().saturating_sub(1);
        } else if ix < self.active {
            self.active -= 1;
        }
        if !self.tabs.is_empty() {
            let active = self.active;
            self.activate(active, window, cx);
        } else {
            window.focus(&self.focus_handle, cx);
        }
        cx.notify();
    }

    fn reload(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ix = self.active;
        let Some(tab) = self.tabs.get(ix) else { return };
        let spec = match &tab.content {
            TabContent::Dataset(doc) => doc.read(cx).dataset.source().cloned(),
            TabContent::Failed { spec, .. } => Some(spec.clone()),
            _ => None,
        };
        let Some(spec) = spec.filter(|s| !s.location.is_empty()) else { return };
        let id = tab.id;
        OpenDatasets::unregister(id, cx);
        let content = self.start_loading(id, spec, window, cx);
        self.tabs[ix].content = content;
        self.tabs[ix]._subscription = None;
        self.ensure_ticker(cx);
        cx.notify();
    }

    fn active_document(&self) -> Option<&Entity<DatasetDocument>> {
        match self.tabs.get(self.active).map(|t| &t.content) {
            Some(TabContent::Dataset(doc)) => Some(doc),
            _ => None,
        }
    }

    /// Refresh elapsed times on loading tabs.
    fn ensure_ticker(&mut self, cx: &mut Context<Self>) {
        if self.ticker.is_some() {
            return;
        }
        self.ticker = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| loop {
            cx.background_executor().timer(Duration::from_millis(500)).await;
            let loading = this
                .update(cx, |this, cx| {
                    let busy = this.tabs.iter().any(|t| matches!(t.content, TabContent::Loading { .. }))
                        || this.active_document().is_some_and(|d| d.read(cx).is_busy());
                    cx.notify();
                    if !busy {
                        this.ticker = None;
                    }
                    busy
                })
                .unwrap_or(false);
            if !loading {
                break;
            }
        }));
    }

    fn tab_title(&self, tab: &WorkspaceTab, cx: &App) -> SharedString {
        match &tab.content {
            TabContent::Loading { spec, .. } | TabContent::Failed { spec, .. } => spec.display_name().into(),
            TabContent::Dataset(doc) => doc.read(cx).title(),
            TabContent::Sql(_) => "SQL Console".into(),
            TabContent::Compare(view) => view.read(cx).title(),
        }
    }

    // ------------------------------------------------------------ render

    fn render_title_bar(&self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let tabs: Vec<AnyElement> = self
            .tabs
            .iter()
            .enumerate()
            .map(|(ix, tab)| {
                let active = ix == self.active;
                let title = self.tab_title(tab, cx);
                let icon: AnyElement = match &tab.content {
                    TabContent::Loading { .. } => Spinner::new().xsmall().into_any_element(),
                    TabContent::Failed { .. } => Icon::new(IconName::TriangleAlert).xsmall().text_color(theme.danger).into_any_element(),
                    TabContent::Dataset(doc) if doc.read(cx).dataset.is_remote() => Icon::new(Lucide::Cloud).xsmall().into_any_element(),
                    TabContent::Dataset(_) => Icon::new(Lucide::Sheet).xsmall().into_any_element(),
                    TabContent::Sql(_) => Icon::new(Lucide::SquareTerminal).xsmall().into_any_element(),
                    TabContent::Compare(_) => Icon::new(Lucide::GitCompare).xsmall().into_any_element(),
                };
                let tooltip_text: SharedString = match &tab.content {
                    TabContent::Dataset(doc) => doc
                        .read(cx)
                        .dataset
                        .source()
                        .map(|s| format::display_path(&s.location))
                        .unwrap_or_else(|| title.to_string())
                        .into(),
                    _ => title.clone(),
                };
                h_flex()
                    .id(("workspace-tab", tab.id))
                    .group("tab")
                    .h(rems(1.75))
                    .max_w(rems(16.))
                    .pl_2()
                    .pr_1()
                    .gap_1p5()
                    .rounded(theme.radius)
                    .text_xs()
                    .when(active, |this| {
                        this.bg(theme.background)
                            .border_1()
                            .border_color(theme.border)
                            .text_color(theme.foreground)
                    })
                    .when(!active, |this| {
                        this.text_color(theme.muted_foreground)
                            .hover(|s| s.bg(theme.secondary_hover))
                    })
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_click(cx.listener(move |this, _, window, cx| this.activate(ix, window, cx)))
                    .on_mouse_down(MouseButton::Middle, cx.listener(move |this, _, window, cx| this.close_tab(ix, window, cx)))
                    .tooltip(move |window, cx| gpui_kit::component::tooltip::Tooltip::new(tooltip_text.clone()).build(window, cx))
                    .child(icon)
                    .child(div().min_w_0().truncate().child(title))
                    .child(
                        Button::new(("close-tab", tab.id))
                            .icon(Icon::new(Lucide::X))
                            .xsmall()
                            .ghost()
                            .on_click(cx.listener(move |this, _, window, cx| this.close_tab(ix, window, cx))),
                    )
                    .into_any_element()
            })
            .collect();
        let _ = window;
        TitleBar::new()
            .when_some(self.menu_bar.clone(), |this, menu_bar| {
                this.child(
                    div()
                        .h_full()
                        .flex_shrink_0()
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(menu_bar),
                )
            })
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_1()
                    .overflow_x_hidden()
                    .children(tabs)
                    .child(
                        div().on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation()).child(
                            Button::new("new-tab")
                                .icon(IconName::Plus)
                                .xsmall()
                                .ghost()
                                .tooltip("Open…")
                                .dropdown_menu(|menu, _, _| {
                                    menu.menu("Open File…", Box::new(OpenFile))
                                        .menu("Open Folder…", Box::new(OpenFolder))
                                        .menu("Open S3 Location…", Box::new(OpenS3))
                                        .menu("Open URL or Path…", Box::new(OpenUrl))
                                        .separator()
                                        .menu("New SQL Console", Box::new(NewSqlConsole))
                                        .menu("Compare…", Box::new(Compare))
                                }),
                        ),
                    ),
            )
            .child(
                h_flex()
                    .pr_2()
                    .gap_1()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .child(
                        Button::new("settings")
                            .icon(IconName::Settings2)
                            .xsmall()
                            .ghost()
                            .tooltip_with_action("Settings", &OpenSettings, None)
                            .on_click(|_, window, cx| window.dispatch_action(Box::new(OpenSettings), cx)),
                    ),
            )
    }

    fn render_welcome(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let recents = AppState::settings(cx).recents.clone();
        let action_button = |id: &'static str, icon: Icon, label: &'static str, action: Box<dyn Action>| {
            Button::new(id)
                .icon(icon)
                .label(label)
                .outline()
                .on_click(move |_, window, cx| window.dispatch_action(action.boxed_clone(), cx))
        };
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_6()
            .p_8()
            .child(
                v_flex()
                    .items_center()
                    .gap_1()
                    .child(div().text_3xl().font_weight(FontWeight::BOLD).child("Parquetry"))
                    .child(div().text_color(theme.muted_foreground).child("Open Parquet, CSV, JSON, Arrow, Delta Lake and Iceberg — on disk or in S3.")),
            )
            .child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .justify_center()
                    .child(action_button("welcome-open", Icon::new(IconName::FolderOpen), "Open…", Box::new(OpenFile)))
                    .child(action_button("welcome-folder", Icon::new(IconName::Folder), "Open Folder…", Box::new(OpenFolder)))
                    .child(action_button("welcome-s3", Icon::new(Lucide::Cloud), "Open from S3…", Box::new(OpenS3)))
                    .child(action_button("welcome-sql", Icon::new(Lucide::SquareTerminal), "SQL Console", Box::new(NewSqlConsole))),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child("Or drop files and folders anywhere in this window."),
            )
            .when(!recents.is_empty(), |this| {
                this.child(
                    v_flex()
                        .w(rems(40.))
                        .max_w_full()
                        .gap_1()
                        .child(
                            h_flex()
                                .justify_between()
                                .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).child("Recent"))
                                .child(
                                    Button::new("clear-recents")
                                        .label("Clear")
                                        .xsmall()
                                        .ghost()
                                        .on_click(|_, window, cx| window.dispatch_action(Box::new(ClearRecents), cx)),
                                ),
                        )
                        .children(recents.iter().take(12).enumerate().map(|(ix, recent)| {
                            let remote = parquetry_engine::is_remote(&recent.location);
                            let location = recent.location.clone();
                            h_flex()
                                .id(("recent", ix))
                                .h(rems(2.))
                                .px_2()
                                .gap_2()
                                .rounded(theme.radius)
                                .hover(|s| s.bg(theme.secondary_hover))
                                .cursor_pointer()
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    let mut spec = SourceSpec::new(location.clone());
                                    if let Some(format) = AppState::settings(cx).recents.get(ix).and_then(|r| r.format) {
                                        spec = spec.with_format(format);
                                    }
                                    this.open(spec, window, cx);
                                }))
                                .child(Icon::new(if remote { Lucide::Cloud } else { Lucide::Sheet }).small().text_color(theme.muted_foreground))
                                .child(div().text_sm().child(SourceSpec::new(recent.location.clone()).display_name()))
                                .child(div().flex_1().min_w_0().truncate().text_xs().text_color(theme.muted_foreground).child(format::display_path(&recent.location)))
                                .child({
                                    let location = recent.location.clone();
                                    Button::new(("remove-recent", ix))
                                        .icon(Icon::new(Lucide::X))
                                        .xsmall()
                                        .ghost()
                                        .tooltip("Remove from Recent")
                                        .on_click(move |_, _, cx| {
                                            cx.stop_propagation();
                                            AppState::update_settings(cx, |s| s.remove_recent(&location));
                                            crate::actions::set_menus(cx);
                                        })
                                })
                        })),
                )
            })
    }

    fn render_content(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let Some(tab) = self.tabs.get(self.active) else {
            return self.render_welcome(cx).into_any_element();
        };
        match &tab.content {
            TabContent::Dataset(doc) => doc.clone().into_any_element(),
            TabContent::Sql(panel) => panel.clone().into_any_element(),
            TabContent::Compare(view) => view.clone().into_any_element(),
            TabContent::Loading { spec, started, .. } => {
                let elapsed = started.elapsed();
                v_flex()
                    .size_full()
                    .items_center()
                    .justify_center()
                    .gap_3()
                    .child(Spinner::new().large())
                    .child(div().text_lg().child(format!("Opening {}", spec.display_name())))
                    .child(div().text_sm().text_color(theme.muted_foreground).child(format::display_path(&spec.location)))
                    .when(elapsed > Duration::from_secs(2), |this| {
                        this.child(div().text_xs().text_color(theme.muted_foreground).child(format!(
                            "{} — reading footers of every file; remote files take longer",
                            format::duration_ms(elapsed.as_millis() as u64)
                        )))
                    })
                    .child(
                        Button::new("cancel-open")
                            .label("Cancel")
                            .outline()
                            .on_click(cx.listener(|this, _, window, cx| {
                                let ix = this.active;
                                this.close_tab(ix, window, cx);
                            })),
                    )
                    .into_any_element()
            }
            TabContent::Failed { spec, error } => v_flex()
                .size_full()
                .items_center()
                .justify_center()
                .gap_3()
                .p_8()
                .child(Icon::new(IconName::TriangleAlert).large().text_color(theme.danger))
                .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).child(format!("Couldn’t open {}", spec.display_name())))
                .child(div().max_w(rems(40.)).text_sm().text_center().whitespace_normal().child(error.clone()))
                .child(
                    h_flex()
                        .gap_2()
                        .child(Button::new("retry").label("Try Again").primary().on_click(cx.listener(|this, _, window, cx| this.reload(window, cx))))
                        .child(Button::new("settings-from-error").label("Settings…").outline().on_click(|_, window, cx| {
                            window.dispatch_action(Box::new(OpenSettings), cx)
                        }))
                        .child(Button::new("close-failed").label("Close").ghost().on_click(cx.listener(|this, _, window, cx| {
                            let ix = this.active;
                            this.close_tab(ix, window, cx);
                        }))),
                )
                .into_any_element(),
        }
    }
}

/// A per-process salt so tab ids differ between windows.
fn cx_window_salt() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    thread_local! { static SALT: u64 = NEXT.fetch_add(1, Ordering::Relaxed); }
    SALT.with(|s| *s)
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let title = self
            .tabs
            .get(self.active)
            .map(|t| format!("{} — Parquetry", self.tab_title(t, cx)))
            .unwrap_or_else(|| "Parquetry".into());
        window.set_window_title(&title);
        v_flex()
            .id("workspace")
            .key_context("Workspace")
            .track_focus(&self.focus_handle)
            .size_full()
            .bg(theme.background)
            .text_color(theme.foreground)
            .on_action(cx.listener(|this, _: &OpenFile, window, cx| this.prompt_open(false, window, cx)))
            .on_action(cx.listener(|this, _: &OpenFolder, window, cx| this.prompt_open(true, window, cx)))
            .on_action(cx.listener(|this, _: &OpenS3, window, cx| this.open_location_dialog(true, window, cx)))
            .on_action(cx.listener(|this, _: &OpenUrl, window, cx| this.open_location_dialog(false, window, cx)))
            .on_action(cx.listener(|this, _: &NewSqlConsole, window, cx| this.open_sql_console(window, cx)))
            .on_action(cx.listener(|this, _: &Compare, window, cx| {
                let preselect = this.active_document().map(|d| d.read(cx).dataset.clone());
                this.compare(preselect, window, cx)
            }))
            .on_action(cx.listener(|this, _: &CloseTab, window, cx| {
                if this.tabs.is_empty() {
                    window.remove_window();
                } else {
                    let ix = this.active;
                    this.close_tab(ix, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &NextTab, window, cx| {
                if !this.tabs.is_empty() {
                    let ix = (this.active + 1) % this.tabs.len();
                    this.activate(ix, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &PreviousTab, window, cx| {
                if !this.tabs.is_empty() {
                    let ix = (this.active + this.tabs.len() - 1) % this.tabs.len();
                    this.activate(ix, window, cx);
                }
            }))
            .on_action(cx.listener(|this, _: &Reload, window, cx| this.reload(window, cx)))
            .on_action(cx.listener(|this, action: &OpenRecent, window, cx| {
                if let Some(recent) = AppState::settings(cx).recents.get(action.0).cloned() {
                    let mut spec = SourceSpec::new(recent.location);
                    if let Some(format) = recent.format {
                        spec = spec.with_format(format);
                    }
                    this.open(spec, window, cx);
                }
            }))
            .on_action(|_: &OpenSettings, window, cx| crate::dialogs::settings::open(window, cx))
            .on_action(|_: &About, window, cx| crate::dialogs::info::about(window, cx))
            .on_action(|_: &ShowShortcuts, window, cx| crate::dialogs::info::shortcuts(window, cx))
            .on_action(|_: &ShowHelp, window, cx| crate::dialogs::info::help(window, cx))
            .drag_over::<ExternalPaths>(|style, _, _, cx| style.bg(cx.theme().accent.opacity(0.15)))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, window, cx| {
                for path in paths.paths() {
                    this.open(SourceSpec::new(path.to_string_lossy()), window, cx);
                }
            }))
            .child(self.render_title_bar(window, cx))
            .child(div().flex_1().min_h_0().child(self.render_content(cx)))
    }
}

/// Open a new workspace window, optionally with locations to open.
pub fn open_window(specs: Vec<SourceSpec>, cx: &mut App) -> Option<WeakEntity<Workspace>> {
    // PARQUETRY_WINDOW_BOUNDS="x,y,width,height" places the window (used by UI tests).
    let bounds = std::env::var("PARQUETRY_WINDOW_BOUNDS")
        .ok()
        .and_then(|text| {
            let v: Vec<f32> = text.split(',').filter_map(|n| n.trim().parse().ok()).collect();
            (v.len() == 4).then(|| Bounds::new(point(px(v[0]), px(v[1])), size(px(v[2]), px(v[3]))))
        })
        .unwrap_or_else(|| Bounds::centered(None, size(px(1400.), px(900.)), cx));
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        window_min_size: Some(size(px(720.), px(460.))),
        app_id: Some("io.parquetry.app".into()),
        ..TitleBar::window_options()
    };
    let result = gpui_kit::open_window(options, cx, |window, cx| {
        let workspace = cx.new(|cx| Workspace::new(window, cx));
        workspace.update(cx, |w, cx| {
            window.focus(&w.focus_handle, cx);
            for spec in specs {
                w.open(spec, window, cx);
            }
        });
        workspace
    });
    match result {
        Ok((handle, workspace)) => {
            let weak = workspace.downgrade();
            cx.default_global::<Workspaces>().0.push((handle, weak.clone()));
            Some(weak)
        }
        Err(error) => {
            log::error!("couldn't open window: {error:#}");
            None
        }
    }
}

/// Refresh the in-window menu bars after `cx.set_menus` (no-op on macOS, where
/// the system draws the menu bar).
pub fn reload_menu_bars(cx: &mut App) {
    if cfg!(target_os = "macos") {
        return;
    }
    if let Some(menus) = cx.get_menus() {
        gpui_kit::base::GlobalState::global_mut(cx).set_app_menus(menus);
    }
    // Deferred: menus are rebuilt from inside workspace updates (e.g. recents).
    cx.defer(|cx| {
        let workspaces: Vec<_> = cx.default_global::<Workspaces>().0.iter().map(|(_, w)| w.clone()).collect();
        for workspace in workspaces {
            let _ = workspace.update(cx, |workspace, cx| {
                if let Some(menu_bar) = &workspace.menu_bar {
                    menu_bar.update(cx, |menu_bar, cx| menu_bar.reload(cx));
                }
            });
        }
    });
}

/// Focus the frontmost workspace and dispatch `action` to it. Returns false when
/// there's no workspace window.
pub fn dispatch_to_front_workspace(action: Box<dyn Action>, cx: &mut App) -> bool {
    thread_local! {
        static REDISPATCHING: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    // If the action is still unhandled after we refocus and re-dispatch it, the
    // global handler fires again; stop there instead of looping.
    if REDISPATCHING.with(|r| r.get()) {
        return false;
    }
    let active = cx.active_window();
    let registry = cx.default_global::<Workspaces>();
    registry.0.retain(|(_, weak)| weak.upgrade().is_some());
    let target = registry
        .0
        .iter()
        .find(|(handle, _)| Some(*handle) == active)
        .or_else(|| registry.0.last())
        .cloned();
    let Some((handle, weak)) = target else { return false };
    // This runs inside the window's own action dispatch, where the window can't be
    // re-entered: finish the job once that dispatch has returned.
    cx.defer(move |cx| {
        let Some(workspace) = weak.upgrade() else { return };
        REDISPATCHING.with(|r| r.set(true));
        let _ = handle.update(cx, |_, window, cx| {
            // Focus where the active tab's commands live, then deliver the action
            // straight to that element (window-level dispatch routes by the
            // previous frame's focus).
            workspace.update(cx, |w, cx| {
                let active = w.active;
                w.activate(active, window, cx);
            });
            let target = window.focused(cx).unwrap_or_else(|| workspace.read(cx).focus_handle.clone());
            target.dispatch_action(&*action, window, cx);
        });
        REDISPATCHING.with(|r| r.set(false));
    });
    true
}

/// Open locations in the frontmost window, or a new one.
pub fn open_in_front_window(specs: Vec<SourceSpec>, cx: &mut App) {
    let active = cx.active_window();
    let registry = cx.default_global::<Workspaces>();
    registry.0.retain(|(_, weak)| weak.upgrade().is_some());
    let target = registry
        .0
        .iter()
        .find(|(handle, _)| Some(*handle) == active)
        .or_else(|| registry.0.last())
        .cloned();
    match target {
        Some((handle, weak)) => {
            let _ = handle.update(cx, |_, window, cx| {
                window.activate_window();
                if let Some(workspace) = weak.upgrade() {
                    workspace.update(cx, |w, cx| {
                        for spec in specs {
                            w.open(spec, window, cx);
                        }
                    });
                }
            });
        }
        None => {
            open_window(specs, cx);
        }
    }
}

#[cfg(test)]
impl Workspace {
    pub(crate) fn tab_count(&self) -> usize {
        self.tabs.len()
    }

    pub(crate) fn active_index(&self) -> usize {
        self.active
    }

    /// "loading", "failed", "dataset", "sql" or "compare" for each tab.
    pub(crate) fn tab_kinds(&self) -> Vec<&'static str> {
        self.tabs
            .iter()
            .map(|t| match t.content {
                TabContent::Loading { .. } => "loading",
                TabContent::Failed { .. } => "failed",
                TabContent::Dataset(_) => "dataset",
                TabContent::Sql(_) => "sql",
                TabContent::Compare(_) => "compare",
            })
            .collect()
    }

    pub(crate) fn document(&self, ix: usize) -> Option<Entity<DatasetDocument>> {
        match self.tabs.get(ix).map(|t| &t.content) {
            Some(TabContent::Dataset(doc)) => Some(doc.clone()),
            _ => None,
        }
    }

    pub(crate) fn sql_panel(&self, ix: usize) -> Option<Entity<SqlPanel>> {
        match self.tabs.get(ix).map(|t| &t.content) {
            Some(TabContent::Sql(panel)) => Some(panel.clone()),
            _ => None,
        }
    }

    pub(crate) fn compare_view(&self, ix: usize) -> Option<Entity<CompareView>> {
        match self.tabs.get(ix).map(|t| &t.content) {
            Some(TabContent::Compare(view)) => Some(view.clone()),
            _ => None,
        }
    }

    pub(crate) fn test_close_tab(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.close_tab(ix, window, cx);
    }

    pub(crate) fn test_compare(&mut self, left: Dataset, right: Dataset, keys: Vec<String>, cx: &mut Context<Self>) {
        let id = self.next_id();
        let view = cx.new(|cx| CompareView::new(left, right, CompareOptions { keys }, cx));
        self.tabs.push(WorkspaceTab {
            id,
            content: TabContent::Compare(view),
            _subscription: None,
        });
        self.active = self.tabs.len() - 1;
    }
}
