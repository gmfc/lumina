# LSP PR1 — Conformant handshake + capability gating — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Await `InitializeResult` before `initialized`, parse & store `ServerCapabilities`, gate every feature request on capability, negotiate/store the position encoding, send a richer/honest `initialize` — all non-blocking, driven through the existing `poll()` loop, with per-client message routing.

**Architecture:** No new async runtime — the handshake completes through the per-tick `LspManager::poll()` on the UI thread (message-passing, no new shared locks). Capabilities parse via the crate's existing resilient hand-rolled `serde_json::Value` extraction. Caps + init-id live in the manager's `ClientState`, not on `LspHandle`.

**Tech Stack:** Rust, `serde_json`, `std::thread` + `std::sync::mpsc`, `lsp-types` (dep present, not used for parsing).

## Global Constraints

- Workspace stays green at **every commit**: `cargo fmt --all --check`; `cargo clippy --workspace --all-targets -- -D warnings`; `cargo test --workspace`.
- MSRV `1.88`; `unsafe` forbidden workspace-wide; lints centralized (`lints.workspace = true`).
- `editor-lsp` returns `io::Result` / narrow results — no `thiserror`.
- `editor-builtins` unchanged (no `editor-lsp`/`lsp-types` dep leak). PR1 touches only `editor-lsp` + `editor-app`.
- App-level tests run hermetically (empty `$HOME`).
- Commit messages end with `Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>`. Rebase-only repo; already on branch `lsp-pr1-handshake-capabilities`.
- Honest capability declaration: declare only implemented features.

---

## File Structure

- `crates/lsp/src/lib.rs` — add `PositionEncoding`, `SyncKind`, `ServerCaps`, `Cap` (data + `allows`). *Additive.*
- `crates/lsp/src/client/parse.rs` — add `parse_capabilities(&Value) -> ServerCaps`. *Additive.*
- `crates/lsp/src/client.rs` — add pure `initialize_params(...)`; split `initialize()` → `send_initialize() -> io::Result<i64>` + `send_initialized()`; `spawn` returns `(LspHandle, Receiver, i64)` and takes `client_version`.
- `crates/lsp/src/client/tests.rs` — unit tests for `parse_capabilities`, `ServerCaps::allows`, `initialize_params`.
- `crates/app/src/lsp.rs` — channel `(String, Incoming)`; `ClientState`; `pending` keyed `(String,i64)`; `ensure_started`/`is_ready`/`request_allowed`; `poll()` handshake + routing; `did_open`/`did_change` readiness-gated + return `bool`; `LspManager::new(root, servers, client_version)`. Manager tests.
- `crates/app/src/lsp/requests.rs` — `send_request` takes a `Cap`, gates on `request_allowed`; each `request_*` passes its `Cap`.
- `crates/app/src/app/lsp.rs` — `update_lsp` readiness gate before serialization.
- `crates/app/src/app/lifecycle.rs:68` — pass `env!("CARGO_PKG_VERSION")` to `LspManager::new`.
- `crates/app/src/app/tests/lsp.rs:273` — update the inert-manager test's `new` call.

---

## Task 1: `editor-lsp` capability types + parsers (pure, additive)

**Files:**
- Modify: `crates/lsp/src/lib.rs` (add types near the other models)
- Modify: `crates/lsp/src/client/parse.rs` (add `parse_capabilities`)
- Test: `crates/lsp/src/client/tests.rs`

**Interfaces:**
- Produces: `editor_lsp::{PositionEncoding, SyncKind, ServerCaps, Cap}`, `ServerCaps::allows(Cap) -> bool`, `editor_lsp::client::parse_capabilities(&serde_json::Value) -> ServerCaps`.

- [ ] **Step 1: Write failing tests** in `crates/lsp/src/client/tests.rs` (append):

```rust
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
        "renameProvider": { "prepareProvider": true }
    }});
    let c = parse_capabilities(&full);
    assert_eq!(c.position_encoding, Some(PositionEncoding::Utf8));
    assert_eq!(c.sync_kind, SyncKind::Incremental);
    assert!(c.hover && c.definition && c.type_definition && c.implementation);
    assert!(c.references && c.document_symbol && c.completion && c.rename);
    assert!(c.allows(Cap::Hover) && c.allows(Cap::Rename));

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
    assert!(!c.definition && !c.rename);
    assert!(!c.allows(Cap::Definition));
}

#[test]
fn parse_capabilities_is_resilient_to_garbage() {
    let c = parse_capabilities(&serde_json::json!({}));
    assert!(!c.hover && !c.completion);
    let c = parse_capabilities(&serde_json::json!({ "capabilities": { "hoverProvider": false } }));
    assert!(!c.hover);
}
```

- [ ] **Step 2: Run to verify FAIL**

Run: `cargo test -p editor-lsp parse_capabilities`
Expected: FAIL — `cannot find function parse_capabilities` / `Cap`, `PositionEncoding`, `SyncKind` undefined.

- [ ] **Step 3: Add types** to `crates/lsp/src/lib.rs` (after the `Severity` enum block):

```rust
/// The position encoding negotiated for a connection. LSP defaults to UTF-16; a server may
/// answer UTF-8 (rust-analyzer, clangd). Stored per connection; PR1 only implements UTF-16.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PositionEncoding {
    Utf16,
    Utf8,
}

/// `TextDocumentSyncKind`: how the server wants document changes. Stored on the caps; PR1
/// always sends full text (`didChange` with no range) regardless — incremental is a later PR.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SyncKind {
    None,
    #[default]
    Full,
    Incremental,
}

/// The capability a feature request needs — one per issuable request method. Used to gate a
/// request against the server's advertised `ServerCapabilities` (a request the server can't
/// serve is dropped silently rather than eliciting `-32601` noise).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cap {
    Hover,
    Definition,
    TypeDefinition,
    Implementation,
    References,
    DocumentSymbol,
    Completion,
    Rename,
}

/// The subset of `ServerCapabilities` Lumina currently gates on. Grows as features land
/// (YAGNI): today only the requests the client actually issues are represented.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServerCaps {
    /// `None` means the server did not answer → UTF-16 default (§2.2).
    pub position_encoding: Option<PositionEncoding>,
    pub sync_kind: SyncKind,
    pub hover: bool,
    pub definition: bool,
    pub type_definition: bool,
    pub implementation: bool,
    pub references: bool,
    pub document_symbol: bool,
    pub completion: bool,
    pub rename: bool,
}

impl ServerCaps {
    /// Whether the server advertised support for the feature behind `cap`.
    pub fn allows(&self, cap: Cap) -> bool {
        match cap {
            Cap::Hover => self.hover,
            Cap::Definition => self.definition,
            Cap::TypeDefinition => self.type_definition,
            Cap::Implementation => self.implementation,
            Cap::References => self.references,
            Cap::DocumentSymbol => self.document_symbol,
            Cap::Completion => self.completion,
            Cap::Rename => self.rename,
        }
    }
}
```

- [ ] **Step 4: Add `parse_capabilities`** to `crates/lsp/src/client/parse.rs`:

```rust
use crate::{PositionEncoding, ServerCaps, SyncKind};

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
    let sync_kind = sync_kind(caps.get("textDocumentSync"));
    ServerCaps {
        position_encoding,
        sync_kind,
        hover: present("hoverProvider"),
        definition: present("definitionProvider"),
        type_definition: present("typeDefinitionProvider"),
        implementation: present("implementationProvider"),
        references: present("referencesProvider"),
        document_symbol: present("documentSymbolProvider"),
        completion: present("completionProvider"),
        rename: present("renameProvider"),
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
```

- [ ] **Step 5: Run to verify PASS**

Run: `cargo test -p editor-lsp parse_capabilities`
Expected: PASS (both tests).

- [ ] **Step 6: Full crate gate + commit**

```bash
cargo fmt --all --check && cargo clippy -p editor-lsp --all-targets -- -D warnings && cargo test -p editor-lsp
git add crates/lsp/src/lib.rs crates/lsp/src/client/parse.rs crates/lsp/src/client/tests.rs
git commit -m "lsp: add ServerCaps + parse_capabilities (capability gating types)

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 2: Conformant handshake + manager gating (atomic — workspace-green)

Bundled because the crate `spawn` signature change cascades into the manager; the workspace can only be green once both land. Ordered so tests are written first, then the code they exercise.

**Files:**
- Modify: `crates/lsp/src/client.rs` (handshake split, `initialize_params`, `spawn`)
- Modify: `crates/lsp/src/client/tests.rs` (test `initialize_params`)
- Modify: `crates/app/src/lsp.rs` (manager refactor + tests)
- Modify: `crates/app/src/lsp/requests.rs` (`Cap` gating)
- Modify: `crates/app/src/app/lsp.rs` (`update_lsp` readiness gate)
- Modify: `crates/app/src/app/lifecycle.rs` (construction)
- Modify: `crates/app/src/app/tests/lsp.rs` (inert-test `new` call)

**Interfaces:**
- Consumes: `editor_lsp::{ServerCaps, Cap, client::parse_capabilities}` (Task 1).
- Produces: `LspHandle::send_initialize(root_uri, client_version) -> io::Result<i64>`, `LspHandle::send_initialized() -> io::Result<()>`, `LspClient::spawn(cmd, args, root_uri, client_version) -> io::Result<(LspHandle, Receiver<Incoming>, i64)>`, `editor_lsp::client::initialize_params(root_uri, client_version) -> serde_json::Value`; manager `ClientState`, `ensure_started`/`is_ready`/`request_allowed`, `did_open`/`did_change -> bool`, `LspManager::new(&Path, HashMap<String,Vec<String>>, String)`.

- [ ] **Step 1: Write failing `initialize_params` test** in `crates/lsp/src/client/tests.rs`:

```rust
#[test]
fn initialize_params_are_honest_and_complete() {
    let p = initialize_params("file:///home/g/proj", "9.9.9");
    assert_eq!(p["clientInfo"]["name"], "lumina");
    assert_eq!(p["clientInfo"]["version"], "9.9.9");
    assert_eq!(p["rootUri"], "file:///home/g/proj");
    assert_eq!(p["rootPath"], "/home/g/proj");
    assert_eq!(p["workspaceFolders"][0]["name"], "proj");
    assert_eq!(p["trace"], "off");
    assert_eq!(p["capabilities"]["general"]["positionEncodings"][0], "utf-16");
    // Honest: no snippet engine, no prepareRename, plaintext hover.
    assert_eq!(
        p["capabilities"]["textDocument"]["completion"]["completionItem"]["snippetSupport"],
        false
    );
    assert_eq!(p["capabilities"]["textDocument"]["rename"]["prepareSupport"], false);
    assert_eq!(p["capabilities"]["textDocument"]["hover"]["contentFormat"][0], "plaintext");
    assert_eq!(p["capabilities"]["textDocument"]["definition"]["linkSupport"], true);
}
```

- [ ] **Step 2: Run to verify FAIL**

Run: `cargo test -p editor-lsp initialize_params`
Expected: FAIL — `cannot find function initialize_params`.

- [ ] **Step 3: Implement handshake in `crates/lsp/src/client.rs`.** Add `initialize_params` (pure) and re-export it; replace `initialize` with `send_initialize`/`send_initialized`; change `spawn`.

Add to the `parse` re-export area a pure builder (place `initialize_params` in `client.rs` above `impl LspHandle`, and add `pub use` isn't needed since it's in `client` module — expose as `pub fn` and add to `client/tests.rs` via `use super::*`):

```rust
/// Build the `initialize` request params. Pure (no I/O) so it is unit-tested. Declares only
/// capabilities the client actually implements (honest declaration): utf-16 only, no snippet
/// engine, no prepareRename, plaintext hover. `rootPath`/`workspaceFolders` are derived from
/// `root_uri`.
pub fn initialize_params(root_uri: &str, client_version: &str) -> Value {
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
            "textDocument": {
                "publishDiagnostics": { "relatedInformation": false },
                "hover": { "contentFormat": ["plaintext"] },
                "definition": { "linkSupport": true },
                "typeDefinition": { "linkSupport": true },
                "implementation": { "linkSupport": true },
                "references": {},
                "documentSymbol": { "hierarchicalDocumentSymbolSupport": true },
                "completion": { "completionItem": { "snippetSupport": false } },
                "rename": { "prepareSupport": false }
            }
        }
    })
}
```

Replace the existing `pub fn initialize(&self, root_uri: &str) -> io::Result<()>` block with:

```rust
    /// Send the `initialize` request only (not `initialized`); returns its JSON-RPC id so the
    /// caller can recognize the response and complete the handshake (§3.2 ordering).
    pub fn send_initialize(&self, root_uri: &str, client_version: &str) -> io::Result<i64> {
        self.request("initialize", initialize_params(root_uri, client_version))
    }

    /// Send the `initialized` notification — only after `InitializeResult` has arrived.
    pub fn send_initialized(&self) -> io::Result<()> {
        self.notify("initialized", json!({}))
    }
```

Change `spawn` to send only `initialize` and return the id:

```rust
    pub fn spawn(
        command: &str,
        args: &[String],
        root_uri: &str,
        client_version: &str,
    ) -> io::Result<(LspHandle, Receiver<Incoming>, i64)> {
        // ... unchanged child spawn + stdin/stdout take + reader thread ...
        let handle = LspHandle {
            stdin: Mutex::new(stdin),
            next_id: AtomicI64::new(1),
            child,
        };
        let init_id = handle.send_initialize(root_uri, client_version)?;
        Ok((handle, rx, init_id))
    }
```

(The reader-thread block and stdin/stdout extraction stay exactly as they are; only the trailing handshake call + return tuple change.)

- [ ] **Step 4: Run to verify `initialize_params` PASS (crate)**

Run: `cargo test -p editor-lsp initialize_params`
Expected: PASS. (The workspace won't build yet — the app still calls the old `spawn`. That's fixed in the next steps before any workspace-level command is run.)

- [ ] **Step 5: Write failing manager tests** in `crates/app/src/lsp.rs` (replace the existing `#[cfg(test)] mod tests` block's contents with these — they supersede the two old tests, updated to the `(String,i64)` keying):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use editor_lsp::{Cap, Incoming, ServerCaps};

    fn manager() -> LspManager {
        LspManager::new(Path::new("/tmp"), HashMap::new(), "test".into())
    }

    #[test]
    fn init_response_stores_caps_and_becomes_ready() {
        let mut mgr = manager();
        mgr.state
            .insert("rust".into(), ClientState::Initializing { init_id: 1 });
        let caps = serde_json::json!({ "capabilities": { "hoverProvider": true } });
        mgr.tx
            .send((
                "rust".into(),
                Incoming::Response { id: 1, result: caps, error: None },
            ))
            .unwrap();
        assert!(mgr.poll().is_empty()); // handshake produces no user-facing event
        assert!(mgr.is_ready("rust"));
        assert!(mgr.request_allowed("rust", Cap::Hover));
        assert!(!mgr.request_allowed("rust", Cap::Completion));
    }

    #[test]
    fn request_allowed_requires_running_and_capability() {
        let mut mgr = manager();
        assert!(!mgr.request_allowed("rust", Cap::Hover)); // no state
        mgr.state.insert(
            "rust".into(),
            ClientState::Running(ServerCaps { hover: true, ..Default::default() }),
        );
        assert!(mgr.request_allowed("rust", Cap::Hover));
        assert!(!mgr.request_allowed("rust", Cap::Rename));
        mgr.state.insert("rust".into(), ClientState::Initializing { init_id: 1 });
        assert!(!mgr.request_allowed("rust", Cap::Hover)); // still initializing
    }

    #[test]
    fn colliding_ids_route_per_language() {
        // Two servers both use id 1; the (language,id) key keeps their responses distinct.
        let mut mgr = manager();
        mgr.pending.insert(("rust".into(), 1), Pending::Rename);
        mgr.pending.insert(("py".into(), 1), Pending::Hover);
        mgr.tx
            .send(("py".into(), Incoming::Response { id: 1, result: serde_json::json!("x"), error: None }))
            .unwrap();
        mgr.tx
            .send(("rust".into(), Incoming::Response { id: 1, result: Default::default(), error: None }))
            .unwrap();
        let events = mgr.poll();
        assert!(events.iter().any(|e| matches!(e, LspEvent::Hover(_))));
        assert!(events.iter().any(|e| matches!(e, LspEvent::Rename(_))));
    }

    #[test]
    fn error_response_surfaces_as_error_event() {
        let mut mgr = manager();
        mgr.pending.insert(("rust".into(), 1), Pending::Rename);
        mgr.tx
            .send(("rust".into(), Incoming::Response {
                id: 1,
                result: Default::default(),
                error: Some("rename failed".into()),
            }))
            .unwrap();
        let events = mgr.poll();
        assert!(matches!(events.as_slice(), [LspEvent::Error(m)] if m == "rename failed"));
    }
}
```

- [ ] **Step 6: Refactor `LspManager`** in `crates/app/src/lsp.rs`. Update imports, struct, `new`, add `ClientState`, `ensure_started`/`is_ready`/`request_allowed`, `poll`, `did_open`/`did_change`.

Imports (top of file):

```rust
use editor_lsp::{ServerCaps, /* existing: */ CompletionItem, DiagnosticsUpdate, DocumentSymbol,
    Incoming, Location, LspClient, LspHandle, WorkspaceEdit};
use editor_lsp::client::{parse_capabilities, parse_completion, parse_document_symbols,
    parse_hover, parse_locations, parse_workspace_edit};
```

Add after the `Pending` enum:

```rust
/// Per-connection lifecycle. The full Starting/ShuttingDown/Crashed machine is a later PR;
/// PR1 needs only the Initializing→Running gate that a conformant handshake requires.
enum ClientState {
    /// `initialize` sent; awaiting its response (whose id is `init_id`) to store caps and send
    /// `initialized`.
    Initializing { init_id: i64 },
    /// Handshake complete; feature requests are gated on these capabilities.
    Running(ServerCaps),
}
```

Struct + `new`:

```rust
pub struct LspManager {
    tx: Sender<(String, Incoming)>,
    rx: Receiver<(String, Incoming)>,
    servers: HashMap<String, Vec<String>>,
    clients: HashMap<String, LspHandle>,
    state: HashMap<String, ClientState>,
    failed: HashMap<String, ()>,
    versions: HashMap<String, i64>,
    pending: HashMap<(String, i64), Pending>,
    root_uri: String,
    client_version: String,
}

impl LspManager {
    pub fn new(root: &Path, servers: HashMap<String, Vec<String>>, client_version: String) -> LspManager {
        let (tx, rx) = channel();
        LspManager {
            tx,
            rx,
            servers,
            clients: HashMap::new(),
            state: HashMap::new(),
            failed: HashMap::new(),
            versions: HashMap::new(),
            pending: HashMap::new(),
            root_uri: uri_for(root),
            client_version,
        }
    }
```

Replace `poll`:

```rust
    pub fn poll(&mut self) -> Vec<LspEvent> {
        let mut out = Vec::new();
        while let Ok((lang, msg)) = self.rx.try_recv() {
            match msg {
                Incoming::Diagnostics(u) => out.push(LspEvent::Diagnostics(u)),
                Incoming::Response { id, result, error } => {
                    // Is this the awaited InitializeResult for this connection?
                    let is_init = matches!(
                        self.state.get(&lang),
                        Some(ClientState::Initializing { init_id }) if *init_id == id
                    );
                    if is_init {
                        match error {
                            Some(err) => {
                                // Initialize failed: drop the connection, don't retry.
                                self.clients.remove(&lang);
                                self.state.remove(&lang);
                                self.failed.insert(lang.clone(), ());
                                out.push(LspEvent::Error(format!("initialize failed: {err}")));
                            }
                            None => {
                                let caps = parse_capabilities(&result);
                                if let Some(handle) = self.clients.get(&lang) {
                                    let _ = handle.send_initialized();
                                }
                                self.state.insert(lang.clone(), ClientState::Running(caps));
                            }
                        }
                        continue;
                    }
                    let Some(kind) = self.pending.remove(&(lang.clone(), id)) else {
                        continue;
                    };
                    if let Some(message) = error {
                        out.push(LspEvent::Error(message));
                    } else {
                        out.extend(response_event(kind, &result));
                    }
                }
            }
        }
        out
    }
```

Replace `ensure_client` with `ensure_started` + `is_ready` + `request_allowed`:

```rust
    /// Ensure a connection for `language` is at least started (spawned + Initializing). Returns
    /// whether a connection record now exists (initializing or running). Non-blocking.
    fn ensure_started(&mut self, language: &str) -> bool {
        if self.clients.contains_key(language) {
            return true;
        }
        if self.failed.contains_key(language) {
            return false;
        }
        let Some(cmd) = self.servers.get(language).cloned() else {
            return false;
        };
        let (program, args) = cmd.split_first().map(|(p, a)| (p.clone(), a.to_vec())).unzip();
        let Some(program) = program else {
            return false;
        };
        match LspClient::spawn(&program, &args.unwrap_or_default(), &self.root_uri, &self.client_version) {
            Ok((handle, rx, init_id)) => {
                let tx = self.tx.clone();
                let lang = language.to_string();
                std::thread::spawn(move || {
                    while let Ok(msg) = rx.recv() {
                        if tx.send((lang.clone(), msg)).is_err() {
                            break;
                        }
                    }
                });
                self.clients.insert(language.to_string(), handle);
                self.state.insert(language.to_string(), ClientState::Initializing { init_id });
                true
            }
            Err(_) => {
                self.failed.insert(language.to_string(), ());
                false
            }
        }
    }

    /// True once the handshake completed and the connection is serving requests.
    fn is_ready(&self, language: &str) -> bool {
        matches!(self.state.get(language), Some(ClientState::Running(_)))
    }

    /// Gate: the connection is Running and advertised support for `cap`.
    fn request_allowed(&self, language: &str, cap: editor_lsp::Cap) -> bool {
        matches!(self.state.get(language), Some(ClientState::Running(caps)) if caps.allows(cap))
    }
```

Update `did_open`/`did_change` to gate on readiness and return `bool`:

```rust
    /// Notify the server a document opened. Sends only once the connection is Running; returns
    /// whether the notification was actually sent (so the caller records the sent revision only
    /// on a real send).
    pub fn did_open(&mut self, path: &Path, language: &str, text: &str) -> bool {
        if !self.is_ready(language) {
            return false;
        }
        let uri = uri_for(path);
        self.versions.insert(uri.clone(), 1);
        if let Some(client) = self.clients.get(language) {
            return client.did_open(&uri, language, 1, text).is_ok();
        }
        false
    }

    /// Notify the server a document changed (full sync). Sends only once Running.
    pub fn did_change(&mut self, path: &Path, language: &str, text: &str) -> bool {
        if !self.is_ready(language) {
            return false;
        }
        let uri = uri_for(path);
        let version = self.versions.entry(uri.clone()).or_insert(1);
        *version += 1;
        let v = *version;
        if let Some(client) = self.clients.get(language) {
            return client.did_change(&uri, v, text).is_ok();
        }
        false
    }
```

Also make `ensure_started`/`is_ready`/`request_allowed` reachable from the `requests` submodule — they are `impl LspManager` methods in the same crate module tree, so private `fn` is fine (submodule `mod requests` sees them via `impl LspManager`). No `pub` needed.

- [ ] **Step 7: Gate requests in `crates/app/src/lsp/requests.rs`.** Update `send_request` to take a `Cap` and gate; pass each request's `Cap`:

```rust
use super::*;
use editor_lsp::Cap;

impl LspManager {
    fn send_request<F>(&mut self, language: &str, kind: Pending, cap: Cap, build: F) -> bool
    where
        F: FnOnce(&LspHandle) -> std::io::Result<i64>,
    {
        self.ensure_started(language);
        if !self.request_allowed(language, cap) {
            return false;
        }
        let Some(client) = self.clients.get(language) else {
            return false;
        };
        match build(client) {
            Ok(id) => {
                self.pending.insert((language.to_string(), id), kind);
                true
            }
            Err(_) => false,
        }
    }
    // request_hover: Cap::Hover; request_definition: Cap::Definition;
    // request_implementation: Cap::Implementation (kind Pending::Definition);
    // request_type_definition: Cap::TypeDefinition (kind Pending::Definition);
    // request_completion: Cap::Completion; request_rename: Cap::Rename;
    // request_references: Cap::References; request_document_symbols: Cap::DocumentSymbol.
}
```

Concretely, update each call, e.g.:

```rust
        self.send_request(language, Pending::Hover, Cap::Hover, |c| c.hover(&uri, line, character))
```
```rust
        self.send_request(language, Pending::Definition, Cap::Implementation, |c| c.implementation(&uri, line, character))
```
```rust
        self.send_request(language, Pending::Definition, Cap::TypeDefinition, |c| c.type_definition(&uri, line, character))
```
```rust
        self.send_request(language, Pending::Definition, Cap::Definition, |c| c.definition(&uri, line, character))
```
```rust
        self.send_request(language, Pending::Completion, Cap::Completion, |c| c.completion(&uri, line, character))
```
```rust
        self.send_request(language, Pending::Rename, Cap::Rename, |c| c.rename(&uri, line, character, new_name))
```
```rust
        self.send_request(language, Pending::References, Cap::References, |c| c.references(&uri, line, character))
```
```rust
        self.send_request(language, Pending::DocumentSymbols, Cap::DocumentSymbol, |c| c.document_symbols(&uri))
```

- [ ] **Step 8: App readiness gate in `crates/app/src/app/lsp.rs`** — `update_lsp` ensures the connection is started and only serializes/sends once Running (so `didOpen`/`didChange` land on the first Running tick, no churn while Initializing). Replace the body after the `(path, lang)` binding:

```rust
        let rev = doc.revision;
        // Kick the connection into starting (non-blocking) and wait for the handshake before
        // sending anything: didOpen/didChange are illegal before `initialized`. Once Running the
        // existing None/changed logic sends the first didOpen with current text.
        self.lsp.ensure_started(&lang);
        if !self.lsp.is_ready(&lang) {
            return;
        }
        match self.lsp_sent_revision.get(&id).copied() {
            None => {
                let text = doc.to_string();
                if self.lsp.did_open(&path, &lang, &text) {
                    self.lsp_sent_revision.insert(id, rev);
                }
            }
            Some(sent) if sent != rev => {
                let text = doc.to_string();
                if self.lsp.did_change(&path, &lang, &text) {
                    self.lsp_sent_revision.insert(id, rev);
                }
            }
            _ => {}
        }
```

Make `ensure_started` and `is_ready` callable from `App`: they are on `LspManager`; the app calls `self.lsp.ensure_started(&lang)` / `self.lsp.is_ready(&lang)`. Change their visibility from private `fn` to `pub(crate) fn` in `crates/app/src/lsp.rs` (the `App` is in a sibling module, not the `lsp` submodule).

- [ ] **Step 9: Construction site** — `crates/app/src/app/lifecycle.rs:68`:

```rust
        let lsp = crate::lsp::LspManager::new(
            &editor.workspace.root,
            config.lsp_servers.clone(),
            env!("CARGO_PKG_VERSION").to_string(),
        );
```

- [ ] **Step 10: Inert-manager test** — `crates/app/src/app/tests/lsp.rs:273`:

```rust
    let mut mgr = crate::lsp::LspManager::new(std::path::Path::new("/tmp"), HashMap::new(), "test".into());
```
(The `did_open`/`did_change` calls at lines 282-283 now return `bool`; they already ignore the value — leave them. All `request_*` still return `false` with no server configured.)

- [ ] **Step 11: Run the full workspace gate**

Run:
```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
HOME="$(mktemp -d)" cargo test --workspace
```
Expected: PASS. New manager tests green; `initialize_params` green; existing LSP + app tests green. (Hermetic `HOME` per the app-test requirement.)

- [ ] **Step 12: Commit**

```bash
git add crates/lsp/src/client.rs crates/lsp/src/client/tests.rs crates/app/src/lsp.rs \
        crates/app/src/lsp/requests.rs crates/app/src/app/lsp.rs \
        crates/app/src/app/lifecycle.rs crates/app/src/app/tests/lsp.rs
git commit -m "lsp: conformant initialize handshake + capability gating

Await InitializeResult before sending 'initialized'; parse & store
ServerCapabilities; gate every feature request on the advertised
capability; send richer/honest initialize params. Non-blocking: the
handshake completes through the existing poll() loop. Per-client
(language,id) message routing fixes a latent response id-collision.

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 3: Verify end-to-end (no code)

- [ ] **Step 1:** Re-run the full gate once more from a clean state to confirm determinism:
  `cargo fmt --all --check && cargo clippy --workspace --all-targets -- -D warnings && HOME="$(mktemp -d)" cargo test --workspace`.
- [ ] **Step 2:** Confirm acceptance criteria from the design doc §8 are met (handshake ordering; caps stored; every request gated; richer init params; per-client routing; new tests + all gates green).
- [ ] **Step 3 (optional, manual — not CI):** With a real `rust-analyzer` configured in `config.toml` (`[lsp] rust = "rust-analyzer"`), open a `.rs` file and confirm diagnostics still appear and hover/goto work (the handshake now waits for caps first). Document the result; do not gate CI on it.

---

## Self-Review

**Spec coverage:** §4A async handshake → Task 2 Steps 3,6 (spawn+poll). §4B ServerCaps+gating → Task 1 (types/parse) + Task 2 Steps 6,7 (request_allowed, send_request Cap). §4C init params/encoding → Task 2 Steps 1,3 (initialize_params; positionEncodings utf-16; honest caps; encoding stored via ServerCaps.position_encoding). §4D per-client routing → Task 2 Steps 5,6 ((String,Incoming) channel, (String,i64) pending, colliding_ids test). §4E manager state + readiness gate → Task 2 Steps 6,8. Testing §6 → Task 1 Steps 1–5, Task 2 Steps 1,5. Non-goals untouched. **Covered.**

**Placeholder scan:** No TBD/TODO. The `// request_hover: Cap::Hover ...` block in Step 7 is a mapping legend immediately followed by the concrete per-call code — not a placeholder.

**Type consistency:** `ServerCaps`/`Cap`/`parse_capabilities` names match across Task 1 and Task 2. `ClientState::{Initializing{init_id}, Running(ServerCaps)}` used identically in `poll`, `is_ready`, `request_allowed`, and tests. `spawn` 4-arg / 3-tuple-return matches `ensure_started`. `did_open`/`did_change` `-> bool` matches `update_lsp` usage. `LspManager::new` 3-arg matches all three call sites (lifecycle + two tests).
