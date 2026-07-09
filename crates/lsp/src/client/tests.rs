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
            assert_eq!(error.as_deref(), Some("rename failed"));
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
        "items": [
            {"label": "println!", "insertText": "println!"},
            {"label": "push", "detail": "fn push(&mut self)"}
        ]
    });
    let items = parse_completion(&list);
    assert_eq!(items.len(), 2);
    assert_eq!(items[0].insert_text, "println!");
    assert_eq!(items[1].insert_text, "push"); // falls back to label
    assert_eq!(items[1].detail.as_deref(), Some("fn push(&mut self)"));
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
    assert_eq!(we.changes[0].0, "file:///a.rs");
    assert_eq!(we.changes[0].1[0].new_text, "bar");
}
