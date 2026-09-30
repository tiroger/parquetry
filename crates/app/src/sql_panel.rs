//! SQL editor with results, used as a document tab and as a standalone console.

use std::time::Instant;

use gpui_kit::assets::IconName as Lucide;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Editor, EditorState, TabSize};
use gpui_kit::component::menu::PopupMenuItem;
use gpui_kit::component::resizable::{resizable_panel, v_resizable};
use gpui_kit::component::spinner::Spinner;
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::component::menu::DropdownMenu as _;
use gpui_kit::component::Disableable as _;
use gpui_kit::*;
use parquetry_engine::{Canceller, Dataset, SqlTable, View, run_sql, table_name_for};
use parquetry_grid::{Grid, GridState};

use crate::actions::RunQuery;
use crate::app_state::AppState;
use crate::format;

/// Datasets open anywhere in the app, available to SQL by name.
#[derive(Default)]
pub struct OpenDatasets {
    entries: Vec<(u64, String, Dataset)>,
}

impl Global for OpenDatasets {}

impl OpenDatasets {
    /// Register a dataset under a unique, SQL-friendly name; returns the name.
    pub fn register(key: u64, dataset: &Dataset, cx: &mut App) -> String {
        let base = table_name_for(&dataset.name);
        let registry = cx.default_global::<OpenDatasets>();
        let mut name = base.clone();
        let mut n = 2;
        while registry.entries.iter().any(|(_, existing, _)| *existing == name) {
            name = format!("{base}_{n}");
            n += 1;
        }
        registry.entries.push((key, name.clone(), dataset.clone()));
        name
    }

    pub fn unregister(key: u64, cx: &mut App) {
        cx.default_global::<OpenDatasets>().entries.retain(|(k, _, _)| *k != key);
    }

    pub fn tables(cx: &App) -> Vec<SqlTable> {
        cx.try_global::<OpenDatasets>()
            .map(|r| {
                r.entries
                    .iter()
                    .map(|(_, name, dataset)| SqlTable {
                        name: name.clone(),
                        dataset: dataset.clone(),
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

pub enum SqlEvent {
    OpenResult { dataset: Dataset, title: String },
}

enum Outcome {
    Rows { rows: u64, millis: u64, truncated: bool },
    Message(String, u64),
    Error(String),
}

pub struct SqlPanel {
    context: Option<Dataset>,
    editor: Entity<EditorState>,
    results: Entity<GridState>,
    result: Option<Dataset>,
    running: Option<(Task<()>, Canceller, Instant)>,
    outcome: Option<Outcome>,
    /// Refreshes the elapsed time while a query runs.
    ticker: Option<Task<()>>,
}

impl EventEmitter<SqlEvent> for SqlPanel {}

impl SqlPanel {
    pub fn new(context: Option<Dataset>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let default_sql = match &context {
            Some(_) => "SELECT *\nFROM t\nLIMIT 1000".to_string(),
            None => "-- Query any file or URL directly, e.g.\n-- SELECT * FROM 's3://bucket/path/*.parquet' LIMIT 100\nSELECT 42 AS answer".to_string(),
        };
        let editor = cx.new(|cx| {
            EditorState::new(window, cx)
                .language("sql")
                .line_number(true)
                .folding(false)
                .tab_size(TabSize { tab_size: 2, hard_tabs: false })
                .soft_wrap(false)
                .searchable(true)
                .default_value(default_sql)
        });
        let results = cx.new(|cx| {
            let mut grid = GridState::new(cx);
            grid.set_show_summaries(false, cx);
            grid
        });
        Self {
            context,
            editor,
            results,
            result: None,
            running: None,
            outcome: None,
            ticker: None,
        }
    }

    /// Move keyboard focus to the editor.
    pub fn focus_editor(this: &Entity<Self>, window: &mut Window, cx: &mut App) {
        let editor = this.read(cx).editor.clone();
        editor.update(cx, |e, cx| e.focus(window, cx));
    }

    #[cfg(test)]
    pub fn set_sql(&mut self, sql: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.editor.update(cx, |e, cx| e.set_value(sql.to_string(), window, cx));
    }

    fn tables(&self, cx: &App) -> Vec<SqlTable> {
        let mut tables = Vec::new();
        if let Some(dataset) = &self.context {
            tables.push(SqlTable {
                name: "t".into(),
                dataset: dataset.clone(),
            });
        }
        for table in OpenDatasets::tables(cx) {
            if !tables.iter().any(|t| t.name == table.name) {
                tables.push(table);
            }
        }
        tables
    }

    pub fn run(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let sql = self.editor.read(cx).value().to_string();
        if sql.trim().is_empty() {
            return;
        }
        if let Some((_, canceller, _)) = self.running.take() {
            canceller.cancel();
        }
        AppState::update_settings(cx, |s| s.add_history(&sql));
        let engine = AppState::engine(cx);
        let job = run_sql(&engine, sql, self.tables(cx));
        let canceller = job.canceller();
        let task = cx.spawn_in(window, async move |this, cx| {
            let result = job.await;
            let _ = this.update_in(cx, |this, _, cx| {
                let started = this.running.take().map(|r| r.2);
                let elapsed = started.map(|s| s.elapsed().as_millis() as u64).unwrap_or(0);
                match result {
                    Ok(outcome) => match outcome.result {
                        Some(dataset) => {
                            this.outcome = Some(Outcome::Rows {
                                rows: dataset.row_count,
                                millis: outcome.millis,
                                truncated: outcome.truncated,
                            });
                            let view = View::identity(&dataset);
                            this.results.update(cx, |g, cx| g.set_view(Some(view), false, cx));
                            this.result = Some(dataset);
                        }
                        None => {
                            this.outcome = Some(Outcome::Message(outcome.message.unwrap_or_else(|| "Done".into()), elapsed));
                        }
                    },
                    Err(error) if error.is_cancelled() => {
                        this.outcome = Some(Outcome::Message("Cancelled".into(), elapsed));
                    }
                    Err(error) => this.outcome = Some(Outcome::Error(error.to_string())),
                }
                cx.notify();
            });
        });
        self.running = Some((task, canceller, Instant::now()));
        self.ticker = Some(cx.spawn(async move |this: WeakEntity<Self>, cx: &mut AsyncApp| loop {
            cx.background_executor().timer(std::time::Duration::from_millis(250)).await;
            let still_running = this
                .update(cx, |this, cx| {
                    cx.notify();
                    this.running.is_some()
                })
                .unwrap_or(false);
            if !still_running {
                break;
            }
        }));
        cx.notify();
    }

    /// The latest result, if the last query returned rows.
    #[cfg(test)]
    pub fn result(&self) -> Option<&Dataset> {
        self.result.as_ref()
    }

    /// The latest error message, if the last query failed.
    #[cfg(test)]
    pub fn error(&self) -> Option<&str> {
        match &self.outcome {
            Some(Outcome::Error(message)) => Some(message),
            _ => None,
        }
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        if let Some((_, canceller, _)) = &self.running {
            canceller.cancel();
        }
        cx.notify();
    }

    pub(crate) fn open_result(&mut self, cx: &mut Context<Self>) {
        if let Some(dataset) = &self.result {
            let first_line = self
                .editor
                .read(cx)
                .value()
                .lines()
                .find(|l| !l.trim().is_empty() && !l.trim_start().starts_with("--"))
                .unwrap_or("Query")
                .trim()
                .to_string();
            let title = crate::format::short(&first_line, 40);
            cx.emit(SqlEvent::OpenResult {
                dataset: dataset.clone(),
                title,
            });
        }
    }

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let running = self.running.is_some();
        let history = AppState::settings(cx).sql_history.clone();
        let editor = self.editor.clone();
        let names: Vec<String> = self.tables(cx).into_iter().map(|t| t.name).collect();
        h_flex()
            .px_2()
            .py_1()
            .gap_2()
            .flex_shrink_0()
            .border_b_1()
            .border_color(theme.border)
            .child(
                Button::new("run-sql")
                    .primary()
                    .small()
                    .icon(IconName::Play)
                    .label("Run")
                    .loading(running)
                    .tooltip_with_action("Run query", &RunQuery, Some("SqlEditor"))
                    .on_click(cx.listener(|this, _, window, cx| this.run(window, cx))),
            )
            .when(running, |this| {
                this.child(
                    Button::new("cancel-sql")
                        .small()
                        .ghost()
                        .label("Cancel")
                        .on_click(cx.listener(|this, _, _, cx| this.cancel(cx))),
                )
            })
            .child(
                Button::new("sql-history")
                    .small()
                    .ghost()
                    .icon(Icon::new(Lucide::Clock))
                    .tooltip("History")
                    .disabled(history.is_empty())
                    .dropdown_menu(move |menu, window, _| {
                        let mut menu = menu.min_w(px(360.)).max_h(px(420.)).scrollable(true);
                        for sql in history.iter().take(50) {
                            let label = crate::format::short(&sql.replace('\n', " "), 70);
                            let editor = editor.clone();
                            let sql = sql.clone();
                            menu = menu.item(PopupMenuItem::new(label).on_click(window.listener_for(
                                &editor,
                                move |e: &mut EditorState, _: &ClickEvent, window, cx| e.set_value(sql.clone(), window, cx),
                            )));
                        }
                        menu
                    }),
            )
            .child(div().flex_1())
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .truncate()
                    .child(if names.is_empty() {
                        "Query files directly: SELECT * FROM 'path/to/file.parquet'".to_string()
                    } else {
                        format!("Tables: {}", names.join(", "))
                    }),
            )
    }

    fn render_results(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let status: AnyElement = match (&self.running, &self.outcome) {
            (Some((_, _, started)), _) => h_flex()
                .gap_2()
                .child(Spinner::new().xsmall())
                .child(format!("Running… {}", format::duration_ms(started.elapsed().as_millis() as u64)))
                .into_any_element(),
            (None, Some(Outcome::Rows { rows, millis, truncated })) => {
                let mut text = format!("{} in {}", format::plural(*rows, "row", "rows"), format::duration_ms(*millis));
                if *truncated {
                    text.push_str(" · limited to the first rows (change the limit in Settings)");
                }
                div().child(text).into_any_element()
            }
            (None, Some(Outcome::Message(text, millis))) => div().child(format!("{text} · {}", format::duration_ms(*millis))).into_any_element(),
            (None, Some(Outcome::Error(_))) => div().text_color(theme.danger).child("Query failed").into_any_element(),
            (None, None) => div().child(format!("Press {} to run", crate::actions::key_label("secondary-enter"))).into_any_element(),
        };
        let body: AnyElement = match &self.outcome {
            Some(Outcome::Error(message)) => div()
                .id("sql-error")
                .p_3()
                .text_sm()
                .font_family(theme.mono_font_family.clone())
                .text_color(theme.danger)
                .whitespace_normal()
                .child(message.clone())
                .into_any_element(),
            _ if self.result.is_some() => Grid::new(&self.results).into_any_element(),
            _ => div().into_any_element(),
        };
        v_flex()
            .size_full()
            .child(
                h_flex()
                    .h(rems(2.))
                    .px_3()
                    .gap_2()
                    .flex_shrink_0()
                    .justify_between()
                    .border_b_1()
                    .border_color(theme.border)
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(status)
                    .when(self.result.is_some() && self.running.is_none(), |this| {
                        this.child(
                            Button::new("open-result")
                                .xsmall()
                                .ghost()
                                .icon(IconName::ExternalLink)
                                .label("Open in New Tab")
                                .tooltip("Filter, sort, summarize and export the result")
                                .on_click(cx.listener(|this, _, _, cx| this.open_result(cx))),
                        )
                    }),
            )
            .child(div().flex_1().min_h_0().child(body))
    }
}

impl Render for SqlPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        v_resizable("sql-split")
            .child(
                resizable_panel().size(px(220.)).size_range(px(90.)..px(900.)).child(
                    v_flex()
                        .size_full()
                        .key_context("SqlEditor")
                        .on_action(cx.listener(|this, _: &RunQuery, window, cx| this.run(window, cx)))
                        .child(self.render_toolbar(cx))
                        .child(
                            div().flex_1().min_h_0().child(
                                Editor::new(&self.editor)
                                    .font_family(theme.mono_font_family.clone())
                                    .text_size(theme.mono_font_size)
                                    .bordered(false)
                                    .size_full(),
                            ),
                        ),
                ),
            )
            .child(resizable_panel().child(self.render_results(cx)))
    }
}
