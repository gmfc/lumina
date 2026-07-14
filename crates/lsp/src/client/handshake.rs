//! The `initialize`/`initialized` handshake: the honest capability declaration and the two
//! ordered messages that open a session (§3.2).

use std::io;

use serde_json::{json, Value};

use super::LspHandle;

/// Build the `initialize` request params. Pure (no I/O) so it is unit-tested. Declares only
/// capabilities the client actually implements (honest declaration): utf-16 only, no snippet
/// engine, no prepareRename, plaintext hover. `rootPath`/`workspaceFolders` are derived from
/// `root_uri`.
pub(crate) fn initialize_params(root_uri: &str, client_version: &str) -> Value {
    let root_path = root_uri.strip_prefix("file://").unwrap_or(root_uri);
    let name = root_path
        .rsplit('/')
        .find(|s| !s.is_empty())
        .unwrap_or("root");
    json!({
        "processId": std::process::id(),
        "clientInfo": { "name": "lumina", "version": client_version },
        "rootUri": root_uri,
        "rootPath": root_path,
        "workspaceFolders": [ { "uri": root_uri, "name": name } ],
        "trace": "off",
        "capabilities": {
            "general": { "positionEncodings": ["utf-16"] },
            "window": { "workDoneProgress": true },
            "textDocument": {
                "publishDiagnostics": { "relatedInformation": false },
                "hover": { "contentFormat": ["plaintext"] },
                "signatureHelp": { "signatureInformation": { "parameterInformation": { "labelOffsetSupport": true }, "activeParameterSupport": true } },
                "definition": { "linkSupport": true },
                "typeDefinition": { "linkSupport": true },
                "implementation": { "linkSupport": true },
                "references": {},
                "documentSymbol": { "hierarchicalDocumentSymbolSupport": true },
                "completion": {
                    "contextSupport": true,
                    "completionItem": {
                        "snippetSupport": true,
                        "resolveSupport": { "properties": ["documentation", "detail", "additionalTextEdits"] }
                    }
                },
                "rename": { "prepareSupport": false },
                "formatting": {},
                "diagnostic": { "dynamicRegistration": false, "relatedDocumentSupport": false },
                // Semantic tokens refine tree-sitter, never replace it (§7.1). We request `full`
                // only, in the standard `relative` encoding, over the standard legend so servers
                // map their tokens onto names we style.
                "semanticTokens": {
                    "dynamicRegistration": false,
                    "requests": { "range": false, "full": true },
                    "formats": ["relative"],
                    "augmentsSyntaxTokens": true,
                    "tokenTypes": [
                        "namespace", "type", "class", "enum", "interface", "struct",
                        "typeParameter", "parameter", "variable", "property", "enumMember",
                        "event", "function", "method", "macro", "keyword", "modifier", "comment",
                        "string", "number", "regexp", "operator", "decorator"
                    ],
                    "tokenModifiers": [
                        "declaration", "definition", "readonly", "static", "deprecated",
                        "abstract", "async", "modification", "documentation", "defaultLibrary"
                    ]
                },
                // Inlay hints as virtual text (§7.2). We resolve nothing lazily yet.
                "inlayHint": { "dynamicRegistration": false },
                // Code lens as virtual text (§6.4).
                "codeLens": { "dynamicRegistration": false },
                // Folding ranges (§7.3); line-only (we ignore fold char columns).
                "foldingRange": { "dynamicRegistration": false, "lineFoldingOnly": true }
            },
            "workspace": {
                // The client owns file watching and forwards matching changes (§8.1); the rest
                // are honestly declared because the manager/app already answer them.
                "applyEdit": true,
                "configuration": true,
                "workspaceFolders": true,
                "didChangeWatchedFiles": { "dynamicRegistration": true, "relativePatternSupport": true },
                "executeCommand": { "dynamicRegistration": false },
                "codeLens": { "refreshSupport": true },
                "inlayHint": { "refreshSupport": true },
                "semanticTokens": { "refreshSupport": true }
            }
        }
    })
}

impl LspHandle {
    /// Send the `initialize` request only (not `initialized`); returns its JSON-RPC id so the
    /// caller can recognize the response and complete the handshake in order (§3.2): capabilities
    /// must be received before `initialized`, and nothing else may be sent until then.
    pub fn send_initialize(&self, root_uri: &str, client_version: &str) -> io::Result<i64> {
        self.request("initialize", initialize_params(root_uri, client_version))
    }

    /// Send the `initialized` notification — only after `InitializeResult` has arrived.
    pub fn send_initialized(&self) -> io::Result<()> {
        self.notify("initialized", json!({}))
    }
}
