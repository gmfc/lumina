# LSP PR2 — Server→client request handling

**Date:** 2026-07-13
**Status:** Approved (roadmap PR2; user directed proceed-to-completion)
**Crates:** `editor-lsp`, `editor-app`.

## Problem

`classify()` recognizes only `publishDiagnostics` + responses; every server→client
**request** is dropped. Per guide §1.3 that silence **deadlocks** any server awaiting the
reply (pyright config pulls, rust-analyzer reload prompts, `workspaceFolders`,
`workDoneProgress/create`, `applyEdit`, refreshes). PR2 answers them all.

## Design

**Crate (`editor-lsp`):**
- `Incoming` gains `ServerRequest { id: serde_json::Value, method: String, params: Value }`
  (id kept raw — server ids may be strings, echoed verbatim per §1.3) and
  `Notification { method: String, params: Value }`.
- `classify`: `method`+`id` → `ServerRequest`; `method` only (non-diagnostics) →
  `Notification`; `publishDiagnostics` stays `Diagnostics`; `id`+result/error → `Response`.
- `LspHandle::respond(id: &Value, result)` / `respond_err(id: &Value, code, message)` write
  JSON-RPC responses. Pure builders `json_response`/`json_error` are unit-tested.

**Manager (`crates/app/src/lsp.rs`) — answers in `poll()`:**
- Manager-local (respond directly via the handle): `workspace/configuration` → array of
  `null` matching `items` arity (we hold no per-server settings yet — correct arity is what
  matters, wrong arity wedges pyright); `workspace/workspaceFolders` → the single root
  folder; `client/registerCapability`/`unregisterCapability`, `window/workDoneProgress/create`,
  `workspace/{semanticTokens,inlayHint,codeLens,diagnostic}/refresh` → `null`; unknown method
  → `-32601`. Pure helpers `configuration_response`/`workspace_folders_response` are tested.
- App-needing (emit `LspEvent::ServerRequest { lang, id, method, params }` for the app to act
  + respond): `workspace/applyEdit`, `window/showMessageRequest`, `window/showDocument`.
- Notifications: `window/showMessage` → `LspEvent::Message(text)` (statusline);
  `logMessage`/`$/progress`/`telemetry/event`/unknown → dropped.
- `LspManager::respond(lang, id, result)` looks up the handle and replies.

**App (`crates/app/src/app/lsp.rs`) — `handle_server_request`:**
- `workspace/applyEdit`: `parse_workspace_edit(params.edit)` → primitive `LspWorkspaceEdit`
  (extract shared `to_primitive_workspace_edit`, reused by the `Rename` arm) → resolve URIs →
  `apply_workspace_edit` (through `Transaction`, invariant #1) → respond `{applied: true}`.
- `window/showMessageRequest`: statusline the message, respond `null` (no action chosen).
- `window/showDocument`: `file:` URI → queue open, respond `{success: true}`; else
  `{success: false}`.

## Non-goals (later)
Storing/honoring dynamic registrations (respond-null only here); `$/progress` UI; message
action buttons; `showDocument` selection/external-open; validate-before-apply failure
reporting for `applyEdit` (respond optimistic `{applied:true}`).

## Testing
- Crate: `classify` → ServerRequest (string id) / Notification; `json_response`/`json_error`
  shape.
- Manager: `configuration_response` arity; `workspace_folders_response` shape; app-needing
  ServerRequest → `LspEvent::ServerRequest` emitted; `window/showMessage` → `LspEvent::Message`.
- App: `to_primitive_workspace_edit` conversion; an applyEdit ServerRequest mutates the doc.
- Gates green; verify no regression against clangd handshake.
