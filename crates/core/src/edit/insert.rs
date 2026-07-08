//! Basic insert and delete operations applied to every selection.

use crate::document::Document;
use crate::history::GroupBreak;
use crate::motion::{self, Motion};

use super::apply::edit_selections;

/// Insert `text` at every caret (replacing any selected span).
pub fn insert_text(doc: &mut Document, text: &str, group: GroupBreak) {
    edit_selections(doc, |_d, sel| (sel.span(), text.to_string()), group);
}

/// Insert a single typed char (coalesces into the current undo group).
pub fn insert_char(doc: &mut Document, ch: char) {
    let mut buf = [0u8; 4];
    let s = ch.encode_utf8(&mut buf);
    insert_text(doc, s, GroupBreak::None);
}

/// Insert a newline at every caret, breaking the undo group.
pub fn insert_newline(doc: &mut Document) {
    insert_text(doc, "\n", GroupBreak::Force);
}

/// Delete the char (grapheme) before each caret, or the selection if non-empty.
pub fn delete_backward(doc: &mut Document) {
    edit_selections(
        doc,
        |d, sel| {
            if sel.is_empty() {
                let from = motion::resolve(d, sel.head, Motion::Left, 1);
                (from..sel.head, String::new())
            } else {
                (sel.span(), String::new())
            }
        },
        GroupBreak::Force,
    );
}

/// Delete the char after each caret, or the selection if non-empty.
pub fn delete_forward(doc: &mut Document) {
    edit_selections(
        doc,
        |d, sel| {
            if sel.is_empty() {
                let to = motion::resolve(d, sel.head, Motion::Right, 1);
                (sel.head..to, String::new())
            } else {
                (sel.span(), String::new())
            }
        },
        GroupBreak::Force,
    );
}

/// Delete the word before each caret (Ctrl+Backspace), or the selection if non-empty.
pub fn delete_word_backward(doc: &mut Document) {
    edit_selections(
        doc,
        |d, sel| {
            if sel.is_empty() {
                let from = motion::resolve(d, sel.head, Motion::WordLeft, 1);
                (from..sel.head, String::new())
            } else {
                (sel.span(), String::new())
            }
        },
        GroupBreak::Force,
    );
}
