//! Parse navigation/query results: hover, goto locations, symbols, signature help, highlights,
//! and code actions.

use serde_json::Value;

use crate::{CodeAction, Command, DocumentHighlight, DocumentSymbol, Location, SignatureHelp};

use super::edits::parse_workspace_edit;
use super::shared::parse_command;

/// Parse a `textDocument/codeAction` result (`(Command | CodeAction)[]`), keeping the actions that
/// carry an `edit` we can apply directly. Command-only actions (executed server-side via
/// `workspace/executeCommand`) and edit-less resolve actions are skipped for now.
pub fn parse_code_actions(result: &Value) -> Vec<CodeAction> {
    let Some(arr) = result.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|a| {
            let title = a.get("title")?.as_str()?.to_string();
            // A bare `Command` has a top-level string `command`; a `CodeAction` has `edit?` and/or
            // a nested `command` object.
            if let Some(cmd) = a.get("command").and_then(|c| c.as_str()) {
                return Some(CodeAction {
                    title,
                    edit: None,
                    command: Some(Command {
                        command: cmd.to_string(),
                        arguments: a.get("arguments").cloned().unwrap_or(Value::Null),
                    }),
                });
            }
            let edit = a
                .get("edit")
                .map(parse_workspace_edit)
                .filter(|e| !e.changes.is_empty());
            let command = a.get("command").and_then(parse_command);
            (edit.is_some() || command.is_some()).then_some(CodeAction {
                title,
                edit,
                command,
            })
        })
        .collect()
}

/// Parse a `workspace/symbol` result (`WorkspaceSymbol[]` / `SymbolInformation[]`) into
/// `(name, location)` pairs. A `WorkspaceSymbol` whose `location` carries only a `uri` (no range,
/// pending `workspaceSymbol/resolve`) defaults to the file start. Malformed entries are skipped.
pub fn parse_workspace_symbols(result: &Value) -> Vec<(String, Location)> {
    let Some(arr) = result.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|s| {
            let name = s.get("name")?.as_str()?.to_string();
            let loc = s.get("location")?;
            let uri = loc.get("uri")?.as_str()?.to_string();
            let pos = |field: &str, key: &str| {
                loc.get("range")
                    .and_then(|r| r.get(field))
                    .and_then(|p| p.get(key))
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0) as u32
            };
            Some((
                name,
                Location {
                    uri,
                    line: pos("start", "line"),
                    character: pos("start", "character"),
                    end_line: pos("end", "line"),
                    end_character: pos("end", "character"),
                },
            ))
        })
        .collect()
}

/// Parse a `textDocument/documentHighlight` result into occurrence ranges. Malformed entries are
/// skipped; a missing `kind` defaults to 1 (Text).
pub fn parse_document_highlights(result: &Value) -> Vec<DocumentHighlight> {
    let Some(arr) = result.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|h| {
            let range = h.get("range")?;
            let start = range.get("start")?;
            let end = range.get("end")?;
            Some(DocumentHighlight {
                line: start.get("line")?.as_u64()? as u32,
                start_char16: start.get("character")?.as_u64()? as u32,
                end_line: end.get("line")?.as_u64()? as u32,
                end_char16: end.get("character")?.as_u64()? as u32,
                kind: h.get("kind").and_then(|k| k.as_u64()).unwrap_or(1) as u8,
            })
        })
        .collect()
}

/// Parse a `textDocument/signatureHelp` result into the active signature + active-parameter
/// range the UI renders. `None` = nothing to show (server said the cursor isn't in a call).
pub fn parse_signature_help(result: &Value) -> Option<SignatureHelp> {
    let sigs = result.get("signatures")?.as_array()?;
    if sigs.is_empty() {
        return None;
    }
    let active_sig = result
        .get("activeSignature")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as usize;
    let sig = sigs.get(active_sig).or_else(|| sigs.first())?;
    let label = sig.get("label")?.as_str()?.to_string();
    // A per-signature activeParameter overrides the top-level one (§5.3).
    let active_idx = sig
        .get("activeParameter")
        .and_then(|v| v.as_u64())
        .or_else(|| result.get("activeParameter").and_then(|v| v.as_u64()))
        .map(|n| n as usize);
    let active_param = active_idx
        .and_then(|idx| sig.get("parameters")?.as_array()?.get(idx).cloned())
        .and_then(|p| param_range(&p, &label));
    Some(SignatureHelp {
        label,
        active_param,
    })
}

/// The char range of a `ParameterInformation.label` within the signature label: either explicit
/// `[start, end]` offsets (we declare `labelOffsetSupport`; treated as char offsets — signatures
/// are effectively ASCII) or a substring to locate.
fn param_range(param: &Value, label: &str) -> Option<(usize, usize)> {
    match param.get("label")? {
        Value::Array(arr) if arr.len() == 2 => {
            let s = arr[0].as_u64()? as usize;
            let e = arr[1].as_u64()? as usize;
            (s <= e && e <= label.chars().count()).then_some((s, e))
        }
        Value::String(sub) => {
            let byte = label.find(sub.as_str())?;
            let start = label[..byte].chars().count();
            Some((start, start + sub.chars().count()))
        }
        _ => None,
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
