//! Parsers that turn raw JSON-RPC response/notification payloads into this crate's models.
//!
//! Split out of [`super`] (the transport/handle machinery); the public parsers are re-exported
//! from there so external paths (`editor_lsp::client::parse_*`) are unchanged.

use serde_json::Value;

use crate::{
    CompletionItem, Diagnostic, DiagnosticsUpdate, DocumentSymbol, Location, PositionEncoding,
    ServerCaps, Severity, SyncKind, TextEdit, WorkspaceEdit,
};

/// Parse an `InitializeResult` into the caps Lumina gates on. Resilient: a provider is
/// "present" when it is `true` or an options object; absent/`false`/`null` means unsupported.
/// `textDocumentSync` is a number (0/1/2) or an object with a `change` number. Unknown shapes
/// fall back to conservative defaults rather than erroring.
pub fn parse_capabilities(init_result: &Value) -> ServerCaps {
    let caps = init_result.get("capabilities").unwrap_or(&Value::Null);
    let present = |key: &str| -> bool {
        match caps.get(key) {
            Some(Value::Bool(b)) => *b,
            Some(Value::Object(_)) => true,
            _ => false,
        }
    };
    let position_encoding = caps
        .get("positionEncoding")
        .and_then(|v| v.as_str())
        .and_then(|s| match s {
            "utf-8" | "utf8" => Some(PositionEncoding::Utf8),
            "utf-16" | "utf16" => Some(PositionEncoding::Utf16),
            _ => None,
        });
    ServerCaps {
        position_encoding,
        sync_kind: sync_kind(caps.get("textDocumentSync")),
        hover: present("hoverProvider"),
        definition: present("definitionProvider"),
        type_definition: present("typeDefinitionProvider"),
        implementation: present("implementationProvider"),
        references: present("referencesProvider"),
        document_symbol: present("documentSymbolProvider"),
        completion: present("completionProvider"),
        rename: present("renameProvider"),
        formatting: present("documentFormattingProvider"),
    }
}

/// Parse a bare `TextEdit[]` result (e.g. `textDocument/formatting`) into our edit model.
/// Malformed entries are skipped rather than discarding the whole batch.
pub fn parse_text_edits(result: &Value) -> Vec<TextEdit> {
    result
        .as_array()
        .map(|arr| arr.iter().filter_map(parse_text_edit).collect())
        .unwrap_or_default()
}

/// Decode `textDocumentSync`: a bare number, or an object's `change` number. Absent/unknown
/// defaults to `Full` (safe: the client always sends full text in PR1).
fn sync_kind(v: Option<&Value>) -> SyncKind {
    let n = match v {
        Some(Value::Number(n)) => n.as_u64(),
        Some(Value::Object(_)) => v.and_then(|o| o.get("change")).and_then(|c| c.as_u64()),
        _ => None,
    };
    match n {
        Some(0) => SyncKind::None,
        Some(2) => SyncKind::Incremental,
        _ => SyncKind::Full,
    }
}

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
            // Skip a single malformed entry rather than discarding the whole batch (which
            // would also fail to clear stale diagnostics for this URI). One buggy or hostile
            // diagnostic must not suppress the valid ones.
            if let Some(diag) = parse_one_diagnostic(d) {
                diagnostics.push(diag);
            }
        }
    }
    Some(DiagnosticsUpdate { uri, diagnostics })
}

/// Parse a single diagnostic object, returning `None` (to be skipped) if it is malformed.
fn parse_one_diagnostic(d: &Value) -> Option<Diagnostic> {
    let range = d.get("range")?;
    let start = range.get("start")?;
    let end = range.get("end")?;
    let severity = match d.get("severity").and_then(|s| s.as_u64()) {
        Some(1) => Severity::Error,
        Some(2) => Severity::Warning,
        Some(3) => Severity::Info,
        _ => Severity::Hint,
    };
    Some(Diagnostic {
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
    })
}
