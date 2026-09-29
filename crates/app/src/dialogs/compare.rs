//! Compare: pick two open datasets and optional key columns.

use std::rc::Rc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogFooter};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::select::{Select, SelectState};
use gpui_kit::component::{ActiveTheme as _, IndexPath, WindowExt as _, v_flex};
use gpui_kit::*;
use parquetry_engine::{CompareOptions, Dataset, SqlTable};

use crate::sql_panel::OpenDatasets;

type OnCompare = Rc<dyn Fn(Dataset, Dataset, CompareOptions, &mut Window, &mut App)>;

pub struct CompareForm {
    tables: Vec<SqlTable>,
    left: Entity<SelectState<Vec<SharedString>>>,
    right: Entity<SelectState<Vec<SharedString>>>,
    keys: Entity<InputState>,
}

impl Render for CompareForm {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let muted = cx.theme().muted_foreground;
        let label = |text: &str| div().text_xs().text_color(muted).child(text.to_string());
        v_flex()
            .gap_3()
            .child(v_flex().gap_1().child(label("Left")).child(Select::new(&self.left)))
            .child(v_flex().gap_1().child(label("Right")).child(Select::new(&self.right)))
            .child(v_flex().gap_1().child(label("Key columns (optional, comma-separated)")).child(Input::new(&self.keys)))
            .child(div().text_xs().text_color(muted).child(
                "With keys, rows are matched by key and changed values are reported per column. Without keys, whole rows are compared.",
            ))
    }
}

pub fn open(preselect: Option<Dataset>, on_compare: impl Fn(Dataset, Dataset, CompareOptions, &mut Window, &mut App) + 'static, window: &mut Window, cx: &mut App) {
    let tables = OpenDatasets::tables(cx);
    if tables.len() < 2 {
        window.push_notification("Open two datasets to compare them.", cx);
        return;
    }
    let names: Vec<SharedString> = tables.iter().map(|t| SharedString::from(format!("{} ({})", t.dataset.name, t.name))).collect();
    let left_ix = preselect
        .as_ref()
        .and_then(|d| tables.iter().position(|t| t.dataset.id == d.id))
        .unwrap_or(0);
    let right_ix = if left_ix == 0 { 1 } else { 0 };
    let on_compare: OnCompare = Rc::new(on_compare);
    let form = cx.new(|cx| CompareForm {
        left: cx.new(|cx| SelectState::new(names.clone(), Some(IndexPath::new(left_ix)), window, cx)),
        right: cx.new(|cx| SelectState::new(names.clone(), Some(IndexPath::new(right_ix)), window, cx)),
        keys: cx.new(|cx| InputState::new(window, cx).placeholder("e.g. id or customer_id, date")),
        tables,
    });
    window.open_dialog(cx, move |dialog, _, _| {
        let form = form.clone();
        let on_compare = on_compare.clone();
        dialog
            .title("Compare Datasets")
            .width(px(520.))
            .child(form.clone())
            .footer(
                DialogFooter::new()
                    .child(DialogClose::new().child(Button::new("cancel").label("Cancel").outline()))
                    .child(DialogAction::new().child(Button::new("compare").label("Compare").primary())),
            )
            .on_ok(move |_, window, cx| {
                let f = form.read(cx);
                let left = f.left.read(cx).selected_index(cx).map(|i| i.row).unwrap_or(0);
                let right = f.right.read(cx).selected_index(cx).map(|i| i.row).unwrap_or(0);
                let keys: Vec<String> = f
                    .keys
                    .read(cx)
                    .value()
                    .split(',')
                    .map(|k| k.trim().to_string())
                    .filter(|k| !k.is_empty())
                    .collect();
                let (Some(l), Some(r)) = (f.tables.get(left), f.tables.get(right)) else { return false };
                let (l, r) = (l.dataset.clone(), r.dataset.clone());
                on_compare(l, r, CompareOptions { keys }, window, cx);
                true
            })
    });
}
