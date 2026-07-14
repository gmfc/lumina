//! Parse an `InitializeResult` into the server capabilities Lumina gates features on.

use serde_json::Value;

use crate::{PositionEncoding, SemanticLegend, ServerCaps, SyncKind};

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
        signature_help: present("signatureHelpProvider"),
        document_highlight: present("documentHighlightProvider"),
        workspace_symbol: present("workspaceSymbolProvider"),
        code_action: present("codeActionProvider"),
        diagnostic: present("diagnosticProvider"),
        diagnostic_identifier: caps
            .get("diagnosticProvider")
            .and_then(|d| d.get("identifier"))
            .and_then(|v| v.as_str())
            .map(String::from),
        // Full-document semantic tokens: the provider must advertise a truthy `full` request
        // (bool `true` or a `{ delta }` object) — we only issue `.../full`, not range/delta.
        semantic_tokens: caps
            .get("semanticTokensProvider")
            .and_then(|p| p.get("full"))
            .is_some_and(|f| f.is_object() || f.as_bool() == Some(true)),
        semantic_legend: caps
            .get("semanticTokensProvider")
            .and_then(|p| p.get("legend"))
            .map(parse_semantic_legend)
            .unwrap_or_default(),
        inlay_hint: present("inlayHintProvider"),
        code_lens: present("codeLensProvider"),
        code_lens_resolve: caps
            .get("codeLensProvider")
            .and_then(|p| p.get("resolveProvider"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        folding_range: present("foldingRangeProvider"),
        execute_commands: caps
            .get("executeCommandProvider")
            .and_then(|e| e.get("commands"))
            .and_then(|c| c.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default(),
    }
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

/// Parse a `semanticTokensProvider.legend` into the ordered type/modifier name lists (§7.1).
fn parse_semantic_legend(legend: &Value) -> SemanticLegend {
    let names = |key: &str| {
        legend
            .get(key)
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default()
    };
    SemanticLegend {
        token_types: names("tokenTypes"),
        token_modifiers: names("tokenModifiers"),
    }
}
