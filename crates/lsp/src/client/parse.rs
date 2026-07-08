//! Parsers that turn raw JSON-RPC response/notification payloads into this crate's models.
//!
//! Split out of [`super`] (the transport/handle machinery); the public parsers are re-exported
//! from there so external paths (`editor_lsp::client::parse_*`) are unchanged.

use serde_json::Value;

use crate::{
    CompletionItem, Diagnostic, DiagnosticsUpdate, DocumentSymbol, Location, Severity, TextEdit,
    WorkspaceEdit,
};

/// Extract hover text from a `textDocument/hover` result. Handles `MarkupContent`,
/// `MarkedString` (string or `{language,value}`), and arrays of those.
pub fn parse_hover(result: &Value) -> Option<String> {
    fn marked_to_string(v: &Value) -> Option<String> {
        if let Some(s) = v.as_str() {
            return Some(s.to_string());
        }
        v.get("value").and_then(|x| x.as_str()).map(String::from)
    }
    let contents = result.get("contents")?;
    let text = if let Some(arr) = contents.as_array() {
        arr.iter()
            .filter_map(marked_to_string)
            .collect::<Vec<_>>()
            .join("\n")
    } else {
        marked_to_string(contents)?
    };
    let text = text.trim().to_string();
    (!text.is_empty()).then_some(text)
}

/// Extract definition location(s). Accepts `Location`, `Location[]`, and `LocationLink[]`.
pub fn parse_locations(result: &Value) -> Vec<Location> {
    fn one(v: &Value) -> Option<Location> {
        // LocationLink uses `targetUri`/`targetSelectionRange`; Location uses `uri`/`range`.
        let (uri, range) = if let Some(uri) = v.get("uri").and_then(|u| u.as_str()) {
            (uri, v.get("range")?)
        } else {
            (
                v.get("targetUri").and_then(|u| u.as_str())?,
                v.get("targetSelectionRange")
                    .or_else(|| v.get("targetRange"))?,
            )
        };
        let start = range.get("start")?;
        let end = range.get("end")?;
        Some(Location {
            uri: uri.to_string(),
            line: start.get("line")?.as_u64()? as u32,
            character: start.get("character")?.as_u64()? as u32,
            end_line: end.get("line")?.as_u64()? as u32,
            end_character: end.get("character")?.as_u64()? as u32,
        })
    }
    match result {
        Value::Array(arr) => arr.iter().filter_map(one).collect(),
        Value::Null => Vec::new(),
        v => one(v).into_iter().collect(),
    }
}

/// Parse a `textDocument/documentSymbol` result. Accepts the hierarchical `DocumentSymbol[]`
/// (with `range`/`selectionRange` + nested `children`, flattened with depth) and the flat
/// `SymbolInformation[]` (with `location.range`). Position is the symbol's selection start.
pub fn parse_document_symbols(result: &Value) -> Vec<DocumentSymbol> {
    let Some(arr) = result.as_array() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for v in arr {
        push_symbol(v, 0, &mut out);
    }
    out
}

fn push_symbol(v: &Value, depth: usize, out: &mut Vec<DocumentSymbol>) {
    let name = v
        .get("name")
        .and_then(|n| n.as_str())
        .unwrap_or("")
        .to_string();
    let kind = v.get("kind").and_then(|k| k.as_u64()).unwrap_or(0) as u8;
    // DocumentSymbol: selectionRange/range at top level. SymbolInformation: location.range.
    let range = v
        .get("selectionRange")
        .or_else(|| v.get("range"))
        .or_else(|| v.get("location").and_then(|l| l.get("range")));
    if let Some(start) = range.and_then(|r| r.get("start")) {
        let line = start.get("line").and_then(|l| l.as_u64()).unwrap_or(0) as u32;
        let character = start.get("character").and_then(|c| c.as_u64()).unwrap_or(0) as u32;
        if !name.is_empty() {
            out.push(DocumentSymbol {
                name,
                kind,
                line,
                character,
                depth,
            });
        }
    }
    if let Some(children) = v.get("children").and_then(|c| c.as_array()) {
        for child in children {
            push_symbol(child, depth + 1, out);
        }
    }
}

/// Extract completion items. Accepts `CompletionItem[]` or `CompletionList {items}`.
pub fn parse_completion(result: &Value) -> Vec<CompletionItem> {
    let items = if let Some(arr) = result.as_array() {
        arr.clone()
    } else if let Some(arr) = result.get("items").and_then(|i| i.as_array()) {
        arr.clone()
    } else {
        return Vec::new();
    };
    items
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
            Some(CompletionItem {
                label,
                detail,
                insert_text,
                kind,
            })
        })
        .collect()
}

/// Parse a rename result (`WorkspaceEdit`). Handles both `changes` and `documentChanges`.
pub fn parse_workspace_edit(result: &Value) -> WorkspaceEdit {
    let mut out = WorkspaceEdit::default();
    let parse_edits =
        |arr: &Vec<Value>| -> Vec<TextEdit> { arr.iter().filter_map(parse_text_edit).collect() };
    if let Some(changes) = result.get("changes").and_then(|c| c.as_object()) {
        for (uri, edits) in changes {
            if let Some(arr) = edits.as_array() {
                out.changes.push((uri.clone(), parse_edits(arr)));
            }
        }
    } else if let Some(docs) = result.get("documentChanges").and_then(|d| d.as_array()) {
        for doc in docs {
            let uri = doc
                .get("textDocument")
                .and_then(|t| t.get("uri"))
                .and_then(|u| u.as_str());
            let edits = doc.get("edits").and_then(|e| e.as_array());
            if let (Some(uri), Some(edits)) = (uri, edits) {
                out.changes.push((uri.to_string(), parse_edits(edits)));
            }
        }
    }
    out
}

fn parse_text_edit(v: &Value) -> Option<TextEdit> {
    let range = v.get("range")?;
    let start = range.get("start")?;
    let end = range.get("end")?;
    Some(TextEdit {
        start_line: start.get("line")?.as_u64()? as u32,
        start_char16: start.get("character")?.as_u64()? as u32,
        end_line: end.get("line")?.as_u64()? as u32,
        end_char16: end.get("character")?.as_u64()? as u32,
        new_text: v.get("newText")?.as_str()?.to_string(),
    })
}

/// Parse a `publishDiagnostics` notification's params into our model.
pub(super) fn parse_diagnostics(value: &Value) -> Option<DiagnosticsUpdate> {
    let params = value.get("params")?;
    let uri = params.get("uri")?.as_str()?.to_string();
    let mut diagnostics = Vec::new();
    if let Some(arr) = params.get("diagnostics").and_then(|d| d.as_array()) {
        for d in arr {
            let range = d.get("range")?;
            let start = range.get("start")?;
            let end = range.get("end")?;
            let severity = match d.get("severity").and_then(|s| s.as_u64()) {
                Some(1) => Severity::Error,
                Some(2) => Severity::Warning,
                Some(3) => Severity::Info,
                _ => Severity::Hint,
            };
            diagnostics.push(Diagnostic {
                line: start.get("line")?.as_u64()? as u32,
                start_char16: start.get("character")?.as_u64()? as u32,
                end_line: end.get("line")?.as_u64()? as u32,
                end_char16: end.get("character")?.as_u64()? as u32,
                severity,
                message: d
                    .get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("")
                    .to_string(),
            });
        }
    }
    Some(DiagnosticsUpdate { uri, diagnostics })
}
