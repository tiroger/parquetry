//! Export: write the current view (filters and sort applied) to a file.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogFooter};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::select::{Select, SelectState};
use gpui_kit::component::{ActiveTheme as _, IndexPath, WindowExt as _, v_flex};
use gpui_kit::*;
use parquetry_engine::{ExportFormat, View, export_view};

use crate::format;

pub struct ExportForm {
    format: Entity<SelectState<Vec<SharedString>>>,
    scope: Entity<SelectState<Vec<SharedString>>>,
    rows: u64,
    hidden: usize,
}

impl ExportForm {
    fn chosen(&self, cx: &App) -> (ExportFormat, bool) {
        let format_ix = self.format.read(cx).selected_index(cx).map(|i| i.row).unwrap_or(0);
        let visible_only = self.scope.read(cx).selected_index(cx).map(|i| i.row).unwrap_or(0) == 1;
        (ExportFormat::all()[format_ix.min(ExportFormat::all().len() - 1)], visible_only)
    }
}

impl Render for ExportForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        v_flex()
            .gap_3()
            .child(div().text_sm().child(format!(
                "{} will be written in the current order, with filters applied.",
                format::plural(self.rows, "row", "rows")
            )))
            .child(v_flex().gap_1().child(div().text_xs().text_color(theme.muted_foreground).child("Format")).child(Select::new(&self.format)))
            .child(v_flex().gap_1().child(div().text_xs().text_color(theme.muted_foreground).child("Columns")).child(Select::new(&self.scope)))
            .child(div().text_xs().text_color(theme.muted_foreground).child(if self.hidden > 0 {
                format!("{} hidden columns are left out of “Visible columns”.", self.hidden)
            } else {
                "Visible columns follow the grid's order, including pinned columns.".to_string()
            }))
    }
}

pub fn open(view: View, visible: Vec<usize>, title: String, window: &mut Window, cx: &mut App) {
    let rows = view.row_count;
    let hidden = view.columns().len() - visible.len();
    let formats: Vec<SharedString> = ExportFormat::all().iter().map(|f| SharedString::from(f.label())).collect();
    let form = cx.new(|cx| ExportForm {
        format: cx.new(|cx| SelectState::new(formats, Some(IndexPath::new(0)), window, cx)),
        scope: cx.new(|cx| {
            SelectState::new(
                vec![SharedString::from("All columns"), SharedString::from("Visible columns, in grid order")],
                Some(IndexPath::new(0)),
                window,
                cx,
            )
        }),
        rows,
        hidden,
    });
    window.open_dialog(cx, move |dialog, _, _| {
        let form = form.clone();
        let view = view.clone();
        let visible = visible.clone();
        let title = title.clone();
        dialog
            .title("Export")
            .width(px(460.))
            .child(form.clone())
            .footer(
                DialogFooter::new()
                    .child(DialogClose::new().child(Button::new("cancel").label("Cancel").outline()))
                    .child(DialogAction::new().child(Button::new("export").label("Export…").primary())),
            )
            .on_ok(move |_, window, cx| {
                let (format, visible_only) = form.read(cx).chosen(cx);
                let columns = visible_only.then(|| visible.clone());
                choose_path_and_export(view.clone(), columns, format, &title, window, cx);
                true
            })
    });
}

fn choose_path_and_export(view: View, columns: Option<Vec<usize>>, format: ExportFormat, title: &str, window: &mut Window, cx: &mut App) {
    let stem = title.rsplit_once('.').map(|(s, _)| s).unwrap_or(title);
    let stem: String = stem.chars().map(|c| if c == '/' || c == ':' { '_' } else { c }).collect();
    let suggested = format!("{stem}.{}", format.extension());
    let directory = dirs::download_dir().or_else(dirs::home_dir).unwrap_or_else(std::env::temp_dir);
    let chosen = cx.prompt_for_new_path(&directory, Some(&suggested));
    let window_handle = window.window_handle();
    cx.spawn(async move |cx: &mut AsyncApp| {
        let Some(path) = chosen.await.ok().and_then(|r| r.ok()).flatten() else { return };
        let path_text = path.to_string_lossy().to_string();
        let _ = cx.update_window(window_handle, |_, window, cx| {
            window.push_notification(
                Notification::info(format!("Writing {}…", format::plural(view.row_count, "row", "rows"))).title("Exporting"),
                cx,
            );
        });
        let result = export_view(&view, columns, path_text.clone(), format).await;
        let _ = cx.update_window(window_handle, |_, window, cx| match result {
            Ok(outcome) => {
                let reveal = path.clone();
                window.push_notification(
                    Notification::success(format!(
                        "{} written to {} in {}",
                        format::plural(outcome.rows, "row", "rows"),
                        format::display_path(&outcome.path),
                        format::duration_ms(outcome.millis)
                    ))
                    .title("Export finished")
                    .autohide(false)
                    .on_click(move |_, _, cx| cx.reveal_path(&reveal)),
                    cx,
                );
            }
            Err(error) => {
                window.push_notification(Notification::error(error.to_string()).title("Export failed").autohide(false), cx);
            }
        });
    })
    .detach();
}
