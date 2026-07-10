//! High-level editing operations that apply to *every* selection at once.
//!
//! This is the one place multi-cursor edits are built: each selection contributes a
//! change, the changes are applied as a single [`crate::Transaction`] (bottom-up, so offsets
//! stay valid — plan "hard part" #3), history is recorded, and the selection set is
//! re-derived from the edited positions.
//!
//! The operations are grouped by concern:
//! - [`apply`] — the multi-selection transaction core every edit builds on.
//! - [`insert`] — plain insert/delete at each caret.
//! - [`smart`] — auto-closing pairs and auto-indent (plan §1.1, §1.2).
//! - [`select`] — selection movement and word/line expansion.
//! - [`linewise`] — line-oriented edits (duplicate, comment, indent, move).
//! - [`hygiene`] — on-save trimming and final-newline insertion.
//! - [`undo`] — undo/redo through the edit layer.

mod apply;
mod helpers;
mod hygiene;
mod insert;
mod linewise;
mod select;
mod smart;
mod undo;

pub use apply::{edit_selections, selection_edit_transaction};
pub use hygiene::apply_save_hygiene;
pub use insert::{
    delete_backward, delete_forward, delete_word_backward, insert_char, insert_newline, insert_text,
};
pub use linewise::{
    copy_line_up, delete_lines, duplicate_line, indent, insert_line_above, insert_line_below,
    move_lines, outdent, toggle_comment,
};
pub use select::{
    cursors_to_line_ends, cursors_to_line_ends_sels, move_selections, select_line, select_word,
};
pub use smart::{delete_backward_smart, insert_char_smart, insert_newline_smart};
pub use undo::{redo, undo};

#[cfg(test)]
mod tests;
