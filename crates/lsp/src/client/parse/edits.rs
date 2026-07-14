//! Parse edit-shaped results: bare `TextEdit[]`, `WorkspaceEdit`, and completion items/edits.

use serde_json::Value;

use crate::{CompletionItem, CompletionList, DocEdit, TextEdit, WorkspaceEdit};

use super::shared::{parse_command, parse_text_edit};

/// Parse a bare `TextEdit[]` result (e.g. `textDocument/formatting`) into our edit model.
/// Malformed entries are skipped rather than discarding the whole batch.
pub fn parse_text_edits(result: &Value) -> Vec<TextEdit> {
    result
        .as_array()
        .map(|arr| arr.iter().filter_map(parse_text_edit).collect())
        .unwrap_or_default()
}

/// Extract completion items. Accepts `CompletionItem[]` or `CompletionList {items}`.
pub fn parse_completion(result: &Value) -> CompletionList {
    // Borrow the array in place — cloning here would deep-copy every completion item (incl.
    // fields we never read) on the per-keystroke completion path (§6/§13).
    let (items, is_incomplete): (&[Value], bool) = if let Some(arr) = result.as_array() {
        (arr, false)
    } else if let Some(arr) = result.get("items").and_then(|i| i.as_array()) {
        let inc = result
            .get("isIncomplete")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        (arr, inc)
    } else {
        return CompletionList::default();
    };
    let items = items
        .iter()
        .filter_map(|it| {
            let label = it.get("label").and_then(|l| l.as_str())?.to_string();
            // Prefer explicit insertText, then a textEdit's newText, else the label.
            let insert_text = it
                .get("insertText")
                .and_then(|t| t.as_str())
                .map(String::from)
                .or_else(|| {
                    it.get("textEdit")
                        .and_then(|e| e.get("newText"))
                        .and_then(|t| t.as_str())
                        .map(String::from)
                })
                .unwrap_or_else(|| label.clone());
            let detail = it.get("detail").and_then(|d| d.as_str()).map(String::from);
            let kind = it.get("kind").and_then(|k| k.as_u64()).map(|k| k as u8);
            let additional_edits = it
                .get("additionalTextEdits")
                .and_then(|a| a.as_array())
                .map(|arr| arr.iter().filter_map(parse_text_edit).collect())
                .unwrap_or_default();
            // insertTextFormat: 1 PlainText, 2 Snippet.
            let is_snippet = it.get("insertTextFormat").and_then(|v| v.as_u64()) == Some(2);
            Some(CompletionItem {
                label,
                detail,
                insert_text,
                kind,
                additional_edits,
                is_snippet,
                data: it.get("data").cloned(),
                command: it.get("command").and_then(parse_command),
            })
        })
        .collect();
    CompletionList {
        items,
        is_incomplete,
    }
}

/// Extract the `additionalTextEdits` of a resolved `completionItem/resolve` item (the auto-import
/// edits that arrive lazily on accept).
pub fn parse_completion_item_additional_edits(result: &Value) -> Vec<TextEdit> {
    result
        .get("additionalTextEdits")
        .and_then(|a| a.as_array())
        .map(|arr| arr.iter().filter_map(parse_text_edit).collect())
        .unwrap_or_default()
}

/// Parse a `WorkspaceEdit` (rename / code action / applyEdit). Prefers `documentChanges` (which
/// carries per-document versions for staleness checking, §2.4) over the legacy `changes` map.
pub fn parse_workspace_edit(result: &Value) -> WorkspaceEdit {
    let mut out = WorkspaceEdit::default();
    let parse_edits =
        |arr: &[Value]| -> Vec<TextEdit> { arr.iter().filter_map(parse_text_edit).collect() };
    // `documentChanges` is preferred (it has versions); fall back to the version-less `changes`.
    if let Some(docs) = result.get("documentChanges").and_then(|d| d.as_array()) {
        for doc in docs {
            let td = doc.get("textDocument");
            let uri = td.and_then(|t| t.get("uri")).and_then(|u| u.as_str());
            let edits = doc.get("edits").and_then(|e| e.as_array());
            if let (Some(uri), Some(edits)) = (uri, edits) {
                out.changes.push(DocEdit {
                    uri: uri.to_string(),
                    // `version` may be a number or null (OptionalVersionedTextDocumentIdentifier).
                    version: td.and_then(|t| t.get("version")).and_then(|v| v.as_i64()),
                    edits: parse_edits(edits),
                });
            }
        }
    } else if let Some(changes) = result.get("changes").and_then(|c| c.as_object()) {
        for (uri, edits) in changes {
            if let Some(arr) = edits.as_array() {
                out.changes.push(DocEdit {
                    uri: uri.clone(),
                    version: None,
                    edits: parse_edits(arr),
                });
            }
        }
    }
    out
}
