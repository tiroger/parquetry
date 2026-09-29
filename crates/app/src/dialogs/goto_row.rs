//! Go to Row: jump to a row number.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::{DialogAction, DialogClose, DialogFooter};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::{ActiveTheme as _, WindowExt as _, v_flex};
use gpui_kit::*;
use parquetry_grid::{CellPos, GridState, Selection};

use crate::format;

/// Parse a row number typed by a person: `1,234,567`, `1_000`, ` 42 `.
pub fn parse_row(text: &str) -> Option<u64> {
    let digits: String = text.chars().filter(|c| !matches!(c, ',' | '_' | ' ')).collect();
    digits.parse().ok()
}

fn go(grid: &Entity<GridState>, text: &str, cx: &mut App) -> bool {
    let Some(row) = parse_row(text) else { return false };
    grid.update(cx, |g, cx| {
        let count = g.row_count();
        if count == 0 {
            return;
        }
        let row = row.min(count - 1);
        let col = g.selection().map(|s| s.head.col).unwrap_or(0);
        g.scroll_to_row(row.saturating_sub(3), cx);
        g.set_selection(Some(Selection::cell(CellPos::new(row, col))), cx);
    });
    true
}

pub fn open(grid: Entity<GridState>, window: &mut Window, cx: &mut App) {
    let count = grid.read(cx).row_count();
    let input = cx.new(|cx| InputState::new(window, cx).placeholder(format!("0 – {}", format::count(count.saturating_sub(1)))));

    crate::dialogs::focus_after_open(input.clone(), window);
    window.open_dialog(cx, move |dialog, _, cx| {
        let grid = grid.clone();
        let ok_input = input.clone();
        dialog
            .title("Go to Row")
            .width(px(360.))
            .child(
                v_flex()
                    .gap_2()
                    .child(Input::new(&input))
                    .child(
                        div()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child("Rows are numbered from 0 in the current view."),
                    ),
            )
            .footer(
                DialogFooter::new()
                    .child(DialogClose::new().child(Button::new("cancel").label("Cancel").outline()))
                    .child(DialogAction::new().child(Button::new("go").label("Go").primary())),
            )
            .on_ok(move |_, window, cx| {
                let text = ok_input.read(cx).value().to_string();
                let done = go(&grid, &text, cx);
                if done {
                    grid.read(cx).focus_handle(cx).focus(window, cx);
                }
                done
            })
    });
}

#[cfg(test)]
mod tests {
    use super::parse_row;

    #[test]
    fn parses_row_numbers() {
        assert_eq!(parse_row("1,234,567"), Some(1_234_567));
        assert_eq!(parse_row(" 42 "), Some(42));
        assert_eq!(parse_row("1_000"), Some(1000));
        assert_eq!(parse_row("abc"), None);
        assert_eq!(parse_row("-1"), None);
    }
}
