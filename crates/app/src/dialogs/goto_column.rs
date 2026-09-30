//! Go to Column: type part of a name to jump to a column (handy for wide tables).

use gpui_kit::component::command::{Command, CommandItem, CommandState};
use gpui_kit::component::{ActiveTheme as _, WindowExt as _, h_flex};
use gpui_kit::*;

use crate::document::DatasetDocument;

/// `grid` is the document's grid (passed in: this runs inside a document update).
pub fn open(doc: Entity<DatasetDocument>, grid: Entity<parquetry_grid::GridState>, window: &mut Window, cx: &mut App) {
    // (column index, name, type, hidden)
    let columns: Vec<(usize, SharedString, SharedString, bool)> = {
        let grid = grid.read(cx);
        grid.columns()
            .iter()
            .enumerate()
            .map(|(ix, column)| {
                (ix, SharedString::from(column.info.name.clone()), SharedString::from(column.info.type_label()), grid.is_hidden(ix))
            })
            .collect()
    };
    let state = cx.new(|cx| CommandState::new(window, cx));
    focus_after_open(state.clone(), window);
    let palette = state.clone();
    window.open_dialog(cx, move |dialog, _, _| {
        let doc = doc.clone();
        let columns_for_confirm: Vec<usize> = columns.iter().map(|c| c.0).collect();
        let items = columns.iter().map(|(_, name, type_label, hidden)| {
            let (name, type_label, hidden) = (name.clone(), type_label.clone(), *hidden);
            CommandItem::new().label(name.clone()).keywords([type_label.clone()]).child(move |_, cx| {
                let muted = cx.theme().muted_foreground;
                h_flex()
                    .w_full()
                    .gap_2()
                    .justify_between()
                    .child(div().min_w_0().truncate().child(name.clone()))
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_xs()
                            .text_color(muted)
                            .child(if hidden { format!("{type_label} · hidden") } else { type_label.to_string() }),
                    )
            })
        });
        dialog.width(px(480.)).p_0().child(
            Command::new(&state)
                .items(items)
                .bordered(false)
                .placeholder("Go to column…")
                .empty(|_, _, cx| div().p_3().text_sm().text_color(cx.theme().muted_foreground).child("No matching column"))
                .on_confirm(move |index, window, cx| {
                    let Some(&column) = columns_for_confirm.get(index.row) else { return };
                    window.close_dialog(cx);
                    doc.update(cx, |doc, cx| doc.reveal_column(column, window, cx));
                }),
        )
    });
    palette.update(cx, |state, cx| state.focus(window, cx));
}

/// Focus the palette once the dialog has rendered (dialogs take focus when they open).
fn focus_after_open(state: Entity<CommandState>, window: &mut Window) {
    window.on_next_frame(move |window, _| {
        window.on_next_frame(move |window, cx| {
            state.update(cx, |state, cx| state.focus(window, cx));
        });
    });
}
