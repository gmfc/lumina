//! On-save buffer hygiene: trailing-whitespace trim and final-newline insertion.

use crate::document::Document;
use crate::transaction::Change;

use super::helpers::apply_line_changes;

/// On-save hygiene (plan §1.4): optionally trim trailing whitespace from every line and/or
/// ensure the buffer ends in a single newline. Applied as one undoable [`crate::Transaction`]
/// *before* the write, so undo restores the pre-save text. Internal storage stays LF — the
/// file's `line_ending` is re-emitted at serialization, never rewritten here (invariant #6).
/// Returns `true` when it changed anything. Selections are mapped through the edit, so a caret
/// sitting past a trimmed line's new end is pulled back to the new EOL.
pub fn apply_save_hygiene(doc: &mut Document, trim_trailing: bool, final_newline: bool) -> bool {
    let before = doc.selections.clone();
    let mut changes: Vec<Change> = Vec::new();

    if trim_trailing {
        for line in 0..doc.len_lines() {
            let text = doc.line_text(line);
            let body = text.trim_end_matches(['\n', '\r']);
            let kept = body.trim_end_matches([' ', '\t']);
            let kept_chars = kept.chars().count();
            let body_chars = body.chars().count();
            if kept_chars < body_chars {
                let line_start = doc.line_to_char(line);
                let start = line_start + kept_chars;
                let end = line_start + body_chars;
                changes.push(Change {
                    at: start,
                    removed: doc.text.slice(start..end).to_string(),
                    inserted: String::new(),
                });
            }
        }
    }

    if final_newline {
        let len = doc.len_chars();
        let ends_with_nl = len > 0 && doc.text.char(len - 1) == '\n';
        if len > 0 && !ends_with_nl {
            changes.push(Change {
                at: len,
                removed: String::new(),
                inserted: "\n".into(),
            });
        }
    }

    if changes.is_empty() {
        return false;
    }
    apply_line_changes(doc, changes, before);
    true
}
