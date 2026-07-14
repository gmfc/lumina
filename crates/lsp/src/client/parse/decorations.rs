//! Parse decoration/overlay results: semantic tokens, inlay hints, code lenses, and folding ranges.

use serde_json::Value;

use crate::{CodeLens, FoldingRange, InlayHint, SemanticLegend, SemanticToken};

/// Decode a `textDocument/semanticTokens/full` result (`{ resultId?, data: uint[] }`) into absolute
/// tokens using `legend` (§7.1). The `data` array is groups of five relative integers
/// `[deltaLine, deltaStartChar, length, typeIdx, modBits]`; `deltaStartChar` is relative to the
/// previous token only when `deltaLine == 0` (else it is an absolute column). Unknown type/modifier
/// indices (shouldn't occur — servers map to our legend) decode to empty names. A malformed tail
/// (length not a multiple of 5) is ignored.
pub fn parse_semantic_tokens(result: &Value, legend: &SemanticLegend) -> Vec<SemanticToken> {
    let Some(data) = result.get("data").and_then(|d| d.as_array()) else {
        return Vec::new();
    };
    let mut out = Vec::with_capacity(data.len() / 5);
    let mut line = 0u32;
    let mut col = 0u32;
    for chunk in data.chunks_exact(5) {
        let n = |v: &Value| v.as_u64().unwrap_or(0) as u32;
        let (delta_line, delta_col, length, type_idx, mod_bits) = (
            n(&chunk[0]),
            n(&chunk[1]),
            n(&chunk[2]),
            n(&chunk[3]),
            n(&chunk[4]),
        );
        if delta_line == 0 {
            col += delta_col;
        } else {
            line += delta_line;
            col = delta_col;
        }
        let token_type = legend
            .token_types
            .get(type_idx as usize)
            .cloned()
            .unwrap_or_default();
        let modifiers = (0..legend.token_modifiers.len())
            .filter(|i| mod_bits & (1 << i) != 0)
            .map(|i| legend.token_modifiers[i].clone())
            .collect();
        out.push(SemanticToken {
            line,
            start_char16: col,
            length,
            token_type,
            modifiers,
        });
    }
    out
}

/// Parse a `textDocument/inlayHint` result into hints (§7.2). `label` is a string or an
/// `InlayHintLabelPart[]` (we flatten the parts' `value`s). `position` is `{ line, character }`
/// (UTF-16). Malformed entries are skipped.
pub fn parse_inlay_hints(result: &Value) -> Vec<InlayHint> {
    let Some(arr) = result.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|h| {
            let pos = h.get("position")?;
            let line = pos.get("line")?.as_u64()? as u32;
            let char16 = pos.get("character")?.as_u64()? as u32;
            let label = match h.get("label")? {
                Value::String(s) => s.clone(),
                Value::Array(parts) => parts
                    .iter()
                    .filter_map(|p| p.get("value").and_then(|v| v.as_str()))
                    .collect(),
                _ => return None,
            };
            Some(InlayHint {
                line,
                char16,
                label,
                kind: h.get("kind").and_then(|k| k.as_u64()).unwrap_or(0) as u8,
                pad_left: h
                    .get("paddingLeft")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
                pad_right: h
                    .get("paddingRight")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false),
            })
        })
        .collect()
}

/// Parse one `CodeLens` object into the model (§6.4): its start position, the resolved `title`
/// (from `command.title`, `None` when unresolved), and the raw JSON to echo to `codeLens/resolve`.
fn parse_one_code_lens(v: &Value) -> Option<CodeLens> {
    let start = v.get("range")?.get("start")?;
    Some(CodeLens {
        line: start.get("line")?.as_u64()? as u32,
        char16: start.get("character")?.as_u64()? as u32,
        title: v
            .get("command")
            .and_then(|c| c.get("title"))
            .and_then(|t| t.as_str())
            .map(String::from),
        raw: v.clone(),
    })
}

/// Parse a `textDocument/codeLens` result (`CodeLens[]`) — malformed entries skipped (§6.4).
pub fn parse_code_lenses(result: &Value) -> Vec<CodeLens> {
    result
        .as_array()
        .map(|arr| arr.iter().filter_map(parse_one_code_lens).collect())
        .unwrap_or_default()
}

/// Parse a `codeLens/resolve` result (a single resolved `CodeLens`, now carrying `command.title`).
pub fn parse_code_lens_resolve(result: &Value) -> Option<CodeLens> {
    parse_one_code_lens(result)
}

/// Parse a `textDocument/foldingRange` result (`FoldingRange[]`) into foldable regions (§7.3).
/// We declared `lineFoldingOnly`, so char columns are ignored. Malformed entries are skipped.
pub fn parse_folding_ranges(result: &Value) -> Vec<FoldingRange> {
    let Some(arr) = result.as_array() else {
        return Vec::new();
    };
    arr.iter()
        .filter_map(|r| {
            Some(FoldingRange {
                start_line: r.get("startLine")?.as_u64()? as u32,
                end_line: r.get("endLine")?.as_u64()? as u32,
                kind: r.get("kind").and_then(|k| k.as_str()).map(String::from),
            })
        })
        .collect()
}
