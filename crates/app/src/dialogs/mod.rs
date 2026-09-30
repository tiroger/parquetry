//! Dialogs: small, focused forms opened over a window.

pub mod compare;
pub mod export;
pub mod filter;
pub mod goto_column;
pub mod goto_row;
pub mod info;
pub mod open_location;
pub mod settings;
pub mod value_counts;

use gpui_kit::component::input::InputState;
use gpui_kit::*;

/// Focus an input once a just-opened dialog has rendered. Dialogs move focus into
/// themselves when they first appear, so focusing earlier doesn't stick.
pub fn focus_after_open(input: Entity<InputState>, window: &mut Window) {
    window.on_next_frame(move |window, _| {
        window.on_next_frame(move |window, cx| {
            input.update(cx, |state, cx| state.focus(window, cx));
        });
    });
}
