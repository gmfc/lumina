use super::*;
use crate::Severity;

#[test]
fn parses_publish_diagnostics_notification() {
    let value: Value = serde_json::from_str(
        r#"{
                "jsonrpc":"2.0",
                "method":"textDocument/publishDiagnostics",
                "params":{
                    "uri":"file:///x/a.rs",
                    "diagnostics":[
                        {"range":{"start":{"line":2,"character":4},"end":{"line":2,"character":9}},
                         "severity":1,"message":"cannot find value"}
                    ]
                }
            }"#,
    )
    .unwrap();
    let update = parse_diagnostics(&value).unwrap();
    assert_eq!(update.uri, "file:///x/a.rs");
    assert_eq!(update.diagnostics.len(), 1);
    let d = &update.diagnostics[0];
    assert_eq!((d.line, d.start_char16, d.end_char16), (2, 4, 9));
    assert_eq!(d.severity, Severity::Error);
}

#[test]
fn raw_diagnostics_stay_in_lockstep_and_preserve_data() {
    // `raw` holds the original JSON of each diagnostic that parsed (skipping the malformed one),
    // so a codeAction context can echo it verbatim — including opaque `data` a quickfix keys off.
    let value: Value = serde_json::from_str(
        r#"{
                "jsonrpc":"2.0",
                "method":"textDocument/publishDiagnostics",
                "params":{
                    "uri":"file:///x/a.rs",
                    "diagnostics":[
                        {"severity":1,"message":"malformed - no range"},
                        {"range":{"start":{"line":1,"character":0},"end":{"line":1,"character":3}},
                         "severity":1,"message":"real","data":{"fix":"import foo"}}
                    ]
                }
            }"#,
    )
    .unwrap();
    let update = parse_diagnostics(&value).unwrap();
    assert_eq!(update.diagnostics.len(), 1);
    assert_eq!(update.raw.len(), 1, "raw must match the parsed count");
    assert_eq!(
        update.raw[0].get("data").and_then(|d| d.get("fix")),
        Some(&Value::String("import foo".into())),
        "opaque `data` must survive verbatim in raw"
    );
}

#[test]
fn malformed_diagnostic_is_skipped_not_whole_batch() {
    // One entry is missing its `range`; the valid entry must still survive.
    let value: Value = serde_json::from_str(
        r#"{
                "jsonrpc":"2.0",
                "method":"textDocument/publishDiagnostics",
                "params":{
                    "uri":"file:///x/a.rs",
                    "diagnostics":[
                        {"severity":1,"message":"malformed - no range"},
                        {"range":{"start":{"line":1,"character":0},"end":{"line":1,"character":3}},
                         "severity":2,"message":"valid"}
                    ]
                }
            }"#,
    )
    .unwrap();
    let update = parse_diagnostics(&value).unwrap();
    assert_eq!(update.diagnostics.len(), 1, "valid diagnostic was dropped");
    assert_eq!(update.diagnostics[0].severity, Severity::Warning);
}

#[test]
fn diagnostic_parses_source_and_code_forms() {
    let make = |diags: serde_json::Value| {
        serde_json::json!({
            "jsonrpc":"2.0","method":"textDocument/publishDiagnostics",
            "params": { "uri":"file:///a.rs", "diagnostics": diags }
        })
    };
    let v = make(serde_json::json!([
        {"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":1}},
         "severity":1,"message":"bad","source":"rustc","code":"E0425"},
        {"range":{"start":{"line":1,"character":0},"end":{"line":1,"character":1}},
         "severity":2,"message":"n","code":42},
        {"range":{"start":{"line":2,"character":0},"end":{"line":2,"character":1}},
         "severity":2,"message":"o","code":{"value":"TS2304","target":"http://x"}}
    ]));
    let ds = parse_diagnostics(&v).unwrap().diagnostics;
    assert_eq!(ds[0].source.as_deref(), Some("rustc"));
    assert_eq!(ds[0].code.as_deref(), Some("E0425"));
    assert_eq!(ds[1].code.as_deref(), Some("42")); // numeric → string
    assert_eq!(ds[2].code.as_deref(), Some("TS2304")); // { value, target } form
    assert!(ds[1].source.is_none());
}

#[test]
fn parses_diagnostic_provider_capability() {
    let caps = serde_json::json!({
        "capabilities": { "diagnosticProvider": { "identifier": "rustc", "interFileDependencies": true } }
    });
    let c = parse_capabilities(&caps);
    assert!(c.diagnostic);
    assert_eq!(c.diagnostic_identifier.as_deref(), Some("rustc"));
    // Absent provider → no pull, no identifier.
    let none = parse_capabilities(&serde_json::json!({ "capabilities": {} }));
    assert!(!none.diagnostic);
    assert!(none.diagnostic_identifier.is_none());
}

#[test]
fn parses_full_and_unchanged_pull_reports() {
    use crate::PullReport;
    // A `full` report carries fresh items (+ raw preserved) and a resultId to cache.
    let full = serde_json::json!({
        "kind": "full",
        "resultId": "r1",
        "items": [
            {"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":1}},
             "severity":1,"message":"boom","data":{"k":1}}
        ]
    });
    match parse_diagnostic_report(&full) {
        PullReport::Full {
            result_id,
            diagnostics,
            raw,
        } => {
            assert_eq!(result_id.as_deref(), Some("r1"));
            assert_eq!(diagnostics.len(), 1);
            assert_eq!(raw.len(), 1);
            assert!(raw[0].get("data").is_some());
        }
        _ => panic!("expected a full report"),
    }
    // An `unchanged` report carries only a resultId; the client keeps its current diagnostics.
    match parse_diagnostic_report(&serde_json::json!({ "kind": "unchanged", "resultId": "r2" })) {
        PullReport::Unchanged { result_id } => assert_eq!(result_id.as_deref(), Some("r2")),
        _ => panic!("expected an unchanged report"),
    }
    // A bare `null` result → an empty full report (clears diagnostics).
    match parse_diagnostic_report(&serde_json::Value::Null) {
        PullReport::Full { diagnostics, .. } => assert!(diagnostics.is_empty()),
        _ => panic!("null should read as an empty full report"),
    }
}

#[test]
fn classify_distinguishes_notification_and_response() {
    let notif = serde_json::from_str::<Value>(
        r#"{"jsonrpc":"2.0","method":"textDocument/publishDiagnostics","params":{"uri":"file:///a","diagnostics":[]}}"#,
    )
    .unwrap();
    assert!(matches!(classify(&notif), Some(Incoming::Diagnostics(_))));

    let resp = serde_json::from_str::<Value>(r#"{"jsonrpc":"2.0","id":7,"result":null}"#).unwrap();
    match classify(&resp) {
        Some(Incoming::Response { id, error, .. }) => {
            assert_eq!(id, 7);
            assert!(
                error.is_none(),
                "a plain result must not look like an error"
            );
        }
        other => panic!("expected response, got {other:?}"),
    }
}

#[test]
fn classify_preserves_error_message() {
    // A JSON-RPC error reply must carry its message, not degrade to a null result — otherwise a
    // failed request (rename, goto, …) is indistinguishable from "no results".
    let err = serde_json::from_str::<Value>(
        r#"{"jsonrpc":"2.0","id":4,"error":{"code":-32603,"message":"rename failed"}}"#,
    )
    .unwrap();
    match classify(&err) {
        Some(Incoming::Response { id, error, .. }) => {
            assert_eq!(id, 4);
            let error = error.expect("error preserved");
            assert_eq!(error.code, -32603);
            assert_eq!(error.message, "rename failed");
        }
        other => panic!("expected error response, got {other:?}"),
    }

    // An error object without a `message` field falls back to its JSON form (still non-None).
    let err2 =
        serde_json::from_str::<Value>(r#"{"jsonrpc":"2.0","id":5,"error":{"code":-1}}"#).unwrap();
    match classify(&err2) {
        Some(Incoming::Response { error, .. }) => assert!(error.is_some()),
        other => panic!("expected error response, got {other:?}"),
    }
}

#[test]
fn hover_handles_markup_and_marked_array() {
    let markup = serde_json::json!({ "contents": { "kind": "markdown", "value": "**x**: i32" } });
    assert_eq!(parse_hover(&markup).as_deref(), Some("**x**: i32"));

    let arr =
        serde_json::json!({ "contents": ["line 1", { "language": "rust", "value": "fn f()" }] });
    assert_eq!(parse_hover(&arr).as_deref(), Some("line 1\nfn f()"));

    assert_eq!(parse_hover(&serde_json::json!({ "contents": "" })), None);
}

#[test]
fn locations_handle_single_array_and_link() {
    let single = serde_json::json!({
        "uri": "file:///a.rs",
        "range": {"start":{"line":3,"character":2},"end":{"line":3,"character":8}}
    });
    let locs = parse_locations(&single);
    assert_eq!(locs.len(), 1);
    assert_eq!((locs[0].line, locs[0].character), (3, 2));

    let link = serde_json::json!([{
        "targetUri": "file:///b.rs",
        "targetSelectionRange": {"start":{"line":10,"character":0},"end":{"line":10,"character":4}}
    }]);
    let locs = parse_locations(&link);
    assert_eq!(locs[0].uri, "file:///b.rs");
    assert_eq!(locs[0].line, 10);
}

#[test]
fn completion_reads_list_and_array_forms() {
    let list = serde_json::json!({
        "isIncomplete": true,
        "items": [
            {"label": "println!", "insertText": "println!"},
            {"label": "push", "detail": "fn push(&mut self)"}
        ]
    });
    let cl = parse_completion(&list);
    assert!(cl.is_incomplete);
    assert_eq!(cl.items.len(), 2);
    assert_eq!(cl.items[0].insert_text, "println!");
    assert_eq!(cl.items[1].insert_text, "push"); // falls back to label
    assert_eq!(cl.items[1].detail.as_deref(), Some("fn push(&mut self)"));
    // A bare array form has no isIncomplete.
    assert!(!parse_completion(&serde_json::json!([{"label": "a"}])).is_incomplete);
}

#[test]
fn completion_parses_additional_edits_snippet_and_data() {
    let list = serde_json::json!({ "items": [{
        "label": "HashMap",
        "insertTextFormat": 2,
        "data": { "id": 7 },
        "additionalTextEdits": [
            {"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":0}},"newText":"use std::collections::HashMap;\n"}
        ]
    }]});
    let it = &parse_completion(&list).items[0];
    assert!(it.is_snippet);
    assert_eq!(it.data, Some(serde_json::json!({ "id": 7 })));
    assert_eq!(it.additional_edits.len(), 1);
    assert_eq!(
        it.additional_edits[0].new_text,
        "use std::collections::HashMap;\n"
    );
}

#[test]
fn document_symbols_hierarchical_and_flat() {
    // Hierarchical `DocumentSymbol[]`: nested children flatten with increasing depth,
    // position taken from selectionRange.
    let hier = serde_json::json!([
        {
            "name": "Foo", "kind": 5,
            "range": {"start": {"line":1,"character":0}, "end": {"line":5,"character":0}},
            "selectionRange": {"start": {"line":1,"character":6}, "end": {"line":1,"character":9}},
            "children": [
                { "name": "bar", "kind": 6,
                  "selectionRange": {"start": {"line":2,"character":4}, "end": {"line":2,"character":7}} }
            ]
        }
    ]);
    let syms = parse_document_symbols(&hier);
    assert_eq!(syms.len(), 2);
    assert_eq!(syms[0].name, "Foo");
    assert_eq!((syms[0].line, syms[0].character, syms[0].depth), (1, 6, 0));
    assert_eq!(syms[1].name, "bar");
    assert_eq!((syms[1].line, syms[1].character, syms[1].depth), (2, 4, 1));

    // Flat `SymbolInformation[]`: position from location.range.
    let flat = serde_json::json!([
        { "name": "main", "kind": 12,
          "location": { "uri": "file:///x",
                        "range": {"start":{"line":0,"character":3},"end":{"line":0,"character":7}} } }
    ]);
    let syms = parse_document_symbols(&flat);
    assert_eq!(syms.len(), 1);
    assert_eq!(syms[0].name, "main");
    assert_eq!((syms[0].line, syms[0].character), (0, 3));
}

#[test]
fn document_highlights_parse_with_kinds() {
    let hls = serde_json::json!([
        {"range":{"start":{"line":1,"character":2},"end":{"line":1,"character":5}},"kind":2},
        {"range":{"start":{"line":3,"character":0},"end":{"line":3,"character":3}},"kind":3},
        {"range":{"start":{"line":5,"character":0},"end":{"line":5,"character":3}}} // no kind → 1
    ]);
    let parsed = parse_document_highlights(&hls);
    assert_eq!(parsed.len(), 3);
    assert_eq!(
        (parsed[0].line, parsed[0].start_char16, parsed[0].kind),
        (1, 2, 2)
    );
    assert_eq!(parsed[1].kind, 3);
    assert_eq!(parsed[2].kind, 1); // default Text
    assert!(parse_document_highlights(&serde_json::json!(null)).is_empty());
}

#[test]
fn workspace_symbols_parse_with_and_without_range() {
    let syms = serde_json::json!([
        {"name":"Foo","kind":5,"location":{"uri":"file:///a.rs",
            "range":{"start":{"line":9,"character":4},"end":{"line":9,"character":7}}}},
        // WorkspaceSymbol with a location that carries only a uri (no range yet).
        {"name":"bar","kind":12,"location":{"uri":"file:///b.rs"}}
    ]);
    let parsed = parse_workspace_symbols(&syms);
    assert_eq!(parsed.len(), 2);
    assert_eq!(parsed[0].0, "Foo");
    assert_eq!((parsed[0].1.line, parsed[0].1.character), (9, 4));
    assert_eq!(parsed[1].0, "bar");
    assert_eq!((parsed[1].1.line, parsed[1].1.character), (0, 0)); // defaulted
    assert_eq!(parsed[1].1.uri, "file:///b.rs");
}

#[test]
fn references_parse_as_locations() {
    let refs = serde_json::json!([
        {"uri":"file:///a.rs","range":{"start":{"line":1,"character":2},"end":{"line":1,"character":5}}},
        {"uri":"file:///b.rs","range":{"start":{"line":9,"character":0},"end":{"line":9,"character":4}}}
    ]);
    let locs = parse_locations(&refs);
    assert_eq!(locs.len(), 2);
    assert_eq!(locs[1].uri, "file:///b.rs");
    assert_eq!((locs[1].line, locs[1].character), (9, 0));
}

#[test]
fn code_actions_keep_edit_and_command_actions() {
    let actions = serde_json::json!([
        // A CodeAction with an edit.
        {"title":"Add import","kind":"quickfix","edit":{"changes":{"file:///a.rs":[
            {"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":0}},"newText":"use x;\n"}
        ]}}},
        // A bare Command (top-level string `command`) → kept as a command action.
        {"title":"Organize Imports","command":"source.organizeImports","arguments":[1]},
        // A CodeAction with a nested command object.
        {"title":"Fix all","command":{"command":"fixAll","arguments":["x"]}},
        // Neither an edit nor a command → skipped.
        {"title":"noop","edit":{"changes":{}}}
    ]);
    let parsed = parse_code_actions(&actions);
    assert_eq!(parsed.len(), 3);
    assert_eq!(parsed[0].title, "Add import");
    assert_eq!(
        parsed[0].edit.as_ref().unwrap().changes[0].uri,
        "file:///a.rs"
    );
    assert!(parsed[0].command.is_none());
    assert_eq!(
        parsed[1].command.as_ref().unwrap().command,
        "source.organizeImports"
    );
    assert!(parsed[1].edit.is_none());
    assert_eq!(parsed[2].command.as_ref().unwrap().command, "fixAll");
    assert!(parse_code_actions(&serde_json::json!(null)).is_empty());
}

#[test]
fn workspace_edit_parses_changes_map() {
    let edit = serde_json::json!({
        "changes": {
            "file:///a.rs": [
                {"range":{"start":{"line":1,"character":4},"end":{"line":1,"character":7}},"newText":"bar"}
            ]
        }
    });
    let we = parse_workspace_edit(&edit);
    assert_eq!(we.changes.len(), 1);
    assert_eq!(we.changes[0].uri, "file:///a.rs");
    assert_eq!(we.changes[0].version, None); // legacy `changes` map has no version
    assert_eq!(we.changes[0].edits[0].new_text, "bar");
}

#[test]
fn workspace_edit_prefers_document_changes_with_version() {
    let edit = serde_json::json!({
        "documentChanges": [
            { "textDocument": { "uri": "file:///a.rs", "version": 7 },
              "edits": [{"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":1}},"newText":"x"}] },
            { "textDocument": { "uri": "file:///b.rs", "version": null },
              "edits": [{"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":0}},"newText":"y"}] }
        ]
    });
    let we = parse_workspace_edit(&edit);
    assert_eq!(we.changes.len(), 2);
    assert_eq!(we.changes[0].uri, "file:///a.rs");
    assert_eq!(we.changes[0].version, Some(7)); // carried for the §2.4 staleness check
    assert_eq!(we.changes[1].version, None); // null version → don't version-check
}

#[test]
fn parse_capabilities_full_and_minimal() {
    use crate::{Cap, PositionEncoding, SyncKind};
    // rust-analyzer-ish: providers as option objects, sync as object, utf-8 offered.
    let full = serde_json::json!({ "capabilities": {
        "positionEncoding": "utf-8",
        "textDocumentSync": { "openClose": true, "change": 2 },
        "hoverProvider": true,
        "definitionProvider": true,
        "typeDefinitionProvider": { "workDoneProgress": true },
        "implementationProvider": true,
        "referencesProvider": true,
        "documentSymbolProvider": true,
        "completionProvider": { "triggerCharacters": ["."] },
        "renameProvider": { "prepareProvider": true },
        "documentFormattingProvider": true,
        "signatureHelpProvider": { "triggerCharacters": ["(", ","] },
        "documentHighlightProvider": true,
        "workspaceSymbolProvider": true,
        "codeActionProvider": { "codeActionKinds": ["quickfix", "refactor"] }
    }});
    let c = parse_capabilities(&full);
    assert_eq!(c.position_encoding, Some(PositionEncoding::Utf8));
    assert_eq!(c.sync_kind, SyncKind::Incremental);
    assert!(c.hover && c.definition && c.type_definition && c.implementation);
    assert!(c.references && c.document_symbol && c.completion && c.rename && c.formatting);
    assert!(c.signature_help && c.document_highlight && c.workspace_symbol && c.code_action);
    assert!(c.allows(Cap::Hover) && c.allows(Cap::Formatting) && c.allows(Cap::SignatureHelp));
    assert!(
        c.allows(Cap::DocumentHighlight)
            && c.allows(Cap::WorkspaceSymbol)
            && c.allows(Cap::CodeAction)
    );

    // minimal: providers as bare booleans, sync as a number, no encoding.
    let min = serde_json::json!({ "capabilities": {
        "textDocumentSync": 1,
        "hoverProvider": true,
        "completionProvider": {}
    }});
    let c = parse_capabilities(&min);
    assert_eq!(c.position_encoding, None); // => utf-16 default
    assert_eq!(c.sync_kind, SyncKind::Full);
    assert!(c.hover && c.completion);
    assert!(!c.definition && !c.rename && !c.formatting);
    assert!(!c.allows(Cap::Definition) && !c.allows(Cap::Formatting));
}

#[test]
fn parse_signature_help_offsets_substring_and_empty() {
    // Offset-based active parameter (labelOffsetSupport).
    let offs = serde_json::json!({
        "signatures": [{ "label": "fn f(a: i32, b: str)", "parameters": [
            { "label": [5, 11] }, { "label": [13, 19] }
        ]}],
        "activeSignature": 0,
        "activeParameter": 1
    });
    let s = parse_signature_help(&offs).unwrap();
    assert_eq!(s.label, "fn f(a: i32, b: str)");
    assert_eq!(s.active_param, Some((13, 19)));

    // Substring-based active parameter, with a per-signature activeParameter override.
    let subs = serde_json::json!({
        "signatures": [{ "label": "add(x, y)", "parameters": [
            { "label": "x" }, { "label": "y" }
        ], "activeParameter": 0 }]
    });
    assert_eq!(
        parse_signature_help(&subs).unwrap().active_param,
        Some((4, 5))
    );

    // No signatures → None (clear the hint).
    assert!(parse_signature_help(&serde_json::json!({ "signatures": [] })).is_none());
}

#[test]
fn parse_text_edits_reads_formatting_result() {
    // textDocument/formatting returns a bare TextEdit[]; malformed entries are skipped.
    let result = serde_json::json!([
        {"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":2}},"newText":""},
        {"bad":"missing range"},
        {"range":{"start":{"line":3,"character":0},"end":{"line":3,"character":0}},"newText":"\n"}
    ]);
    let edits = parse_text_edits(&result);
    assert_eq!(edits.len(), 2);
    assert_eq!(edits[0].start_char16, 0);
    assert_eq!(edits[0].end_char16, 2);
    assert_eq!(edits[1].new_text, "\n");
    assert!(parse_text_edits(&serde_json::json!(null)).is_empty());
}

#[test]
fn parse_capabilities_is_resilient_to_garbage() {
    let c = parse_capabilities(&serde_json::json!({}));
    assert!(!c.hover && !c.completion);
    let c = parse_capabilities(&serde_json::json!({ "capabilities": { "hoverProvider": false } }));
    assert!(!c.hover);
}

#[test]
fn classify_server_request_and_notification() {
    // method + id (string id) => a server→client request to be answered.
    let req = serde_json::from_str::<Value>(
        r#"{"jsonrpc":"2.0","id":"tok-1","method":"window/workDoneProgress/create","params":{"token":"t"}}"#,
    )
    .unwrap();
    match classify(&req) {
        Some(Incoming::ServerRequest { id, method, params }) => {
            assert_eq!(id, serde_json::json!("tok-1")); // raw id preserved (string)
            assert_eq!(method, "window/workDoneProgress/create");
            assert_eq!(params["token"], "t");
        }
        other => panic!("expected ServerRequest, got {other:?}"),
    }

    // method, no id => a notification.
    let notif = serde_json::from_str::<Value>(
        r#"{"jsonrpc":"2.0","method":"window/showMessage","params":{"type":3,"message":"hi"}}"#,
    )
    .unwrap();
    match classify(&notif) {
        Some(Incoming::Notification { method, params }) => {
            assert_eq!(method, "window/showMessage");
            assert_eq!(params["message"], "hi");
        }
        other => panic!("expected Notification, got {other:?}"),
    }

    // A plain response is still a Response (not misread as a request/notification).
    let resp = serde_json::from_str::<Value>(r#"{"jsonrpc":"2.0","id":7,"result":null}"#).unwrap();
    assert!(matches!(
        classify(&resp),
        Some(Incoming::Response { id: 7, .. })
    ));
}

#[test]
fn response_error_droppable_matrix() {
    use crate::ResponseError;
    let drop = |code| {
        ResponseError {
            code,
            message: String::new(),
        }
        .is_droppable()
    };
    assert!(drop(ResponseError::REQUEST_CANCELLED));
    assert!(drop(ResponseError::CONTENT_MODIFIED));
    assert!(drop(ResponseError::SERVER_CANCELLED));
    assert!(!drop(ResponseError::REQUEST_FAILED)); // a real failure is surfaced
    assert!(!drop(-32603)); // InternalError is surfaced
}

#[test]
fn classify_preserves_error_code() {
    let err = serde_json::from_str::<Value>(
        r#"{"jsonrpc":"2.0","id":9,"error":{"code":-32801,"message":"content modified"}}"#,
    )
    .unwrap();
    match classify(&err) {
        Some(Incoming::Response { error, .. }) => {
            let e = error.unwrap();
            assert_eq!(e.code, -32801);
            assert!(e.is_droppable());
        }
        other => panic!("expected response, got {other:?}"),
    }
}

#[test]
fn json_response_and_error_echo_id_verbatim() {
    let ok = json_response(
        &serde_json::json!("s-9"),
        serde_json::json!({ "applied": true }),
    );
    assert_eq!(ok["jsonrpc"], "2.0");
    assert_eq!(ok["id"], "s-9"); // string id echoed as-is
    assert_eq!(ok["result"]["applied"], true);
    assert!(ok.get("error").is_none());

    let err = json_error(&serde_json::json!(4), -32601, "method not found");
    assert_eq!(err["id"], 4);
    assert_eq!(err["error"]["code"], -32601);
    assert_eq!(err["error"]["message"], "method not found");
    assert!(err.get("result").is_none());
}

#[test]
fn initialize_params_are_honest_and_complete() {
    let p = initialize_params("file:///home/g/proj", "9.9.9");
    assert_eq!(p["clientInfo"]["name"], "lumina");
    assert_eq!(p["clientInfo"]["version"], "9.9.9");
    assert_eq!(p["rootUri"], "file:///home/g/proj");
    assert_eq!(p["rootPath"], "/home/g/proj");
    assert_eq!(p["workspaceFolders"][0]["name"], "proj");
    assert_eq!(p["trace"], "off");
    assert_eq!(
        p["capabilities"]["general"]["positionEncodings"][0],
        "utf-16"
    );
    // Honest: we have a real snippet expander, so we declare snippetSupport and the resolve
    // properties we actually apply. Still no prepareRename, plaintext hover.
    assert_eq!(
        p["capabilities"]["textDocument"]["completion"]["completionItem"]["snippetSupport"],
        true
    );
    assert_eq!(
        p["capabilities"]["textDocument"]["completion"]["completionItem"]["resolveSupport"]
            ["properties"],
        serde_json::json!(["documentation", "detail", "additionalTextEdits"])
    );
    assert_eq!(
        p["capabilities"]["textDocument"]["rename"]["prepareSupport"],
        false
    );
    assert_eq!(
        p["capabilities"]["textDocument"]["hover"]["contentFormat"][0],
        "plaintext"
    );
    assert_eq!(
        p["capabilities"]["textDocument"]["definition"]["linkSupport"],
        true
    );
    // Pull diagnostics declared, honestly without related-document support (we don't render it).
    assert_eq!(
        p["capabilities"]["textDocument"]["diagnostic"]["relatedDocumentSupport"],
        false
    );
    // The client owns file watching and answers configuration/workspaceFolders/applyEdit.
    assert_eq!(
        p["capabilities"]["workspace"]["didChangeWatchedFiles"]["dynamicRegistration"],
        true
    );
    assert_eq!(p["capabilities"]["workspace"]["configuration"], true);
}
