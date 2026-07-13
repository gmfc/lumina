# LSP PR1 — Conformant handshake + capability gating

**Date:** 2026-07-13
**Status:** Design approved (handshake mechanism A1; per-client routing bundled)
**Feature crate(s):** `editor-lsp`, `editor-app` (LSP glue). No `editor-builtins` change.

---

## 1. Context — the larger effort

`LSP_CLIENT_IMPLEMENTATION_GUIDE.md` specifies a full LSP 3.17 client at VS Code
parity, as a 6-phase roadmap (§13). Lumina already ships a **minimal, well-architected
Phase-0/1/partial-2 client**: hardened `Content-Length` framing, UTF-16 position math
(both unit-tested), a **threads + `mpsc`** transport (correctly adapted to Lumina's
no-tokio rule — the guide's tokio 4-task topology is *not* the house style), per-language
server spawn, full-sync `didOpen`/`didChange`, `publishDiagnostics` push, and per-feature
**builtin plugins** (hover, definition/impl/typeDef, references, documentSymbols,
completion v0, rename) decoupled from `lsp-types` via **primitive-twin types**, with
`WorkspaceEdit`s applied through `Transaction` (invariant #1).

A gap analysis against the guide found the **foundational blocker**: the `initialize`
response is discarded, so `ServerCapabilities` are never stored and `initialized` is sent
*before* the result arrives. Consequently there is **zero capability gating**, no
`positionEncoding` negotiation, and no sync-kind honoring — every request fires blind.

Because the repo is **one-feature-per-PR / hermetic-gates / rebase-only**, the guide is
decomposed into a PR sequence:

| # | PR | Guide |
|---|-----|-------|
| **1 (this doc)** | **Conformant handshake + capabilities + gating** | §3.2–3.5, §2.2 |
| 2 | Server→client request handling (answer all; default `-32601`) | §1.3, §8.2/8.3/8.6 |
| 3 | Lifecycle robustness (shutdown deadline, fail-pending, crash→restart+resync, `didClose`) | §3.8/3.9 |
| 4 | Cancellation + staleness guards (`$/cancelRequest`, `(uri,version,generation)`) | §1.4/§9.4 |
| 5 | Mock-server transcript harness | §12 L2 |
| 6+ | Feature PRs (signature help, completion overhaul, formatting, code actions, diagnostics enrichment, §7 decorations) | §5–7 |

PR1 is foundation-first: it makes the existing features **correct and honest** and
unblocks PR2–6. It adds **no new user-visible feature**.

## 2. Architecture constraints honored

- **No tokio / never block the input loop** (ARCHITECTURE.md §9, §13). `poll()` and the
  spawn path both run on the UI/render thread, so the handshake must be **non-blocking**,
  driven through the existing per-tick `poll()` — message-passing, no new shared locks.
- **`editor-lsp` returns `io::Result` / narrow results**, not a `thiserror` enum
  (ARCHITECTURE.md §5). Capability parsing follows the crate's existing **resilient
  hand-rolled `serde_json::Value` extraction** (skip-malformed-keep-the-rest), not
  `lsp_types` struct deserialization.
- **No `Arc<Mutex<T>>` sprawl** (§13). The async handshake introduces **no** new shared
  mutable state; caps flow to the app through the existing channel.
- **CI has no language server.** Every unit is testable with `serde_json::Value` fixtures
  and synthetic `Incoming` fed through `poll()` (the established pattern in
  `crates/lsp/src/client/tests.rs` and the `LspManager` tests). App-level tests run
  hermetically (empty `$HOME`).
- **Honest capability declaration** (guide §3.5): declare only what is implemented.

## 3. Goal & non-goals

**Goal.** Await `InitializeResult` before `initialized`; parse & store
`ServerCapabilities`; gate every feature request on the stored capability; negotiate (and
store) the position encoding; send a richer, honest `initialize` payload. Bundle the
per-client message-routing fix required for correct multi-server init.

**Non-goals (later PRs).** Server→client request handling (PR2); cancellation / staleness
guards (PR4); crash/restart/resync and the full connection state machine (PR3);
`didClose` / save family (PR3); incremental `didChange`; config channels; dynamic
registration; **utf-8 position encoding** (we implement only utf-16 today — advertising
utf-16-only is honest; utf-8 is a fast-follow); feature enrichment (PR6+).

## 4. Design

### A. Async handshake via the poll loop (A1)

State per client: `Initializing → Running` (the full `Starting/ShuttingDown/Crashed`
machine is PR3).

1. **`spawn()` sends only `initialize`** and returns `(LspHandle, Receiver, init_id)`; the
   reader thread starts as today. The manager records `ClientState::Initializing { init_id }`.
   It does **not** send `initialized` yet.
2. **`poll()`** already runs each tick. When it sees the response tagged
   `(language, id == init_id)`:
   - `parse_capabilities(&result)` → `ServerCaps`.
   - `handle.send_initialized()`.
   - transition to `ClientState::Running(caps)` — caps live in the manager's state, not on
     the handle (gating happens in the app's `send_request`, which sees manager state).
3. Documents open naturally: `App::update_lsp` only serializes+sends `didOpen`/`didChange`
   once the client is `Running` (see §D), so the first `Running` tick sends `didOpen` with
   current text via the existing `lsp_sent_revision == None` path. **No open-queue and no
   per-tick serialization churn while `Initializing`.**
4. Feature requests issued before `Running` are **dropped** (guide §3.2 — "drop hover, the
   user retriggers"); the client is still kicked into `Initializing` so it becomes ready.

During `Initializing` the only channel traffic is the init response: `classify()` still
drops `logMessage`/`showMessage`/`$/progress` (server→client handling is PR2), so they
cannot clog the handshake.

### B. `ServerCaps` + gating

New in `editor-lsp` (`lib.rs`), parsed in `client/parse.rs`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PositionEncoding { Utf16, Utf8 } // Utf8 stored-but-unused in PR1 (see §C)

/// TextDocumentSyncKind honored for didChange (PR1 stores it; full-sync stays the
/// only implemented mode — Incremental is a later PR).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncKind { None, Full, Incremental }

/// Exactly what today's requests gate on. Grows in later PRs as features land (YAGNI).
#[derive(Debug, Clone, Default)]
pub struct ServerCaps {
    pub position_encoding: Option<PositionEncoding>, // None => Utf16 default
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

/// The capability a request needs — one per issuable request method.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cap { Hover, Definition, TypeDefinition, Implementation,
               References, DocumentSymbol, Completion, Rename }

impl ServerCaps {
    pub fn allows(&self, cap: Cap) -> bool { /* match cap => field */ }
}

/// Resilient parse of an InitializeResult's `capabilities` (skip-malformed style).
/// bool | options-object providers both count as present; textDocumentSync is number OR
/// object; positionEncoding "utf-8"/"utf8"/"utf-16".
pub fn parse_capabilities(init_result: &serde_json::Value) -> ServerCaps;
```

- Caps live in the manager's `ClientState::Running(ServerCaps)` (single source of truth);
  `LspHandle` is not touched for caps or init-id storage.
- Gating happens in the app's `send_request` (`crates/app/src/lsp/requests.rs`): after
  confirming `Running`, `if !caps.allows(cap) { return false }`. An unsupported request
  **degrades silently** (no `-32601` noise). Each `request_*` passes its `Cap`.
  (`implementation`/`type_definition` keep reusing `Pending::Definition` for response
  interpretation but gate on their own `Cap`.)

### C. Init params & encoding (honest)

`initialize` params grow toward the §3.5 baseline, **declaring only what is implemented**:

- Add `clientInfo { name: "lumina", version }` (version threaded from the app —
  `editor-app`'s `CARGO_PKG_VERSION`), `rootPath` (from `root_uri`), `workspaceFolders:
  [{ uri: root_uri, name: <last path segment> }]`, `trace: "off"`,
  `general.positionEncodings: ["utf-16"]`.
- Keep honest: `completion.completionItem.snippetSupport: false` (no snippet engine),
  `rename.prepareSupport: false` (no `prepareRename` yet), `hover.contentFormat:
  ["plaintext"]` (raw-text hover today), `definition.linkSupport: true` (we parse
  `LocationLink`), `documentSymbol.hierarchicalDocumentSymbolSupport: true` (both forms
  handled).
- **Encoding:** advertise **utf-16 only** and read back `capabilities.positionEncoding`,
  storing it on the handle. Conversions stay utf-16 (matching what we advertised). A
  non-conformant server that answers `utf-8` despite our utf-16-only offer is logged and
  treated as utf-16. **utf-8 conversion + advertising utf-8 is an explicit non-goal** (a
  fast-follow that adds the utf-8 fns to `position.rs` and threads the encoding through the
  app's conversion sites).

To keep `spawn`'s signature clean, everything except `client_version` is derived from
`root_uri`; `spawn` gains one `client_version: &str` parameter.

### D. Per-client message routing (bundled correctness fix)

The merged channel carries bare `Incoming`; every client's `next_id` starts at 1, so init
responses (and, latently, feature responses) collide across servers.

- Channel becomes `Sender<(String /*language*/, Incoming)>`; the per-server forwarding
  thread (`ensure_started`) tags each message with its language.
- Correlation map becomes `pending: HashMap<(String, i64), Pending>`.
- `poll()` routes by language: a `(lang, Response { id })` matching that client's
  `init_id` completes the handshake; otherwise `pending.remove(&(lang, id))` interprets the
  feature response.

This is required for correct multi-server init and also kills the pre-existing
feature-response id-collision bug.

### E. Manager state & app readiness gate

`LspManager` gains a minimal per-client state:

```rust
enum ClientState { Initializing { init_id: i64 }, Running(ServerCaps) }
// state: HashMap<String /*language*/, ClientState>, alongside clients: HashMap<_, LspHandle>
```

- `ensure_started(lang) -> bool` — spawn if configured & not previously failed
  (→ `Initializing`); returns whether a client record exists (initializing or running).
- `is_ready(lang) -> bool` — true only when `Running`.
- `did_open` / `did_change` send only when `is_ready` (they return `bool`; the app records
  `lsp_sent_revision` only on a real send — already the shape, just made explicit).
- `send_request` (app): `if !is_ready(lang) { ensure_started(lang); return false }` then
  the `Cap` gate.
- `App::update_lsp`: `ensure_started(&lang); if !self.lsp.is_ready(&lang) { return; }`
  **before** serializing text — so no churn while `Initializing`, and the existing
  `None`/changed logic sends `didOpen`/`didChange` on the first `Running` tick.

## 5. File-by-file change list

- `crates/lsp/src/lib.rs` — add `PositionEncoding`, `SyncKind`, `ServerCaps`, `Cap`. No
  new `Incoming` variant (init result arrives as `Incoming::Response`).
- `crates/lsp/src/client.rs` — split `initialize()` into `send_initialize() -> i64` (called
  by `spawn`, sends `initialize` only) and `send_initialized()`; `spawn` returns
  `(LspHandle, Receiver, init_id)` and gains a `client_version` param; richer params.
  `LspHandle` gains **no** caps/init-id fields (those live in the manager's `ClientState`).
- `crates/lsp/src/client/parse.rs` — `parse_capabilities(&Value) -> ServerCaps`.
- `crates/lsp/src/client/tests.rs` — caps-parse fixtures + `ServerCaps::allows` truth table.
- `crates/app/src/lsp.rs` — channel `(String, Incoming)`; `ClientState` (holds `init_id`
  while Initializing and `ServerCaps` when Running); `pending` re-keyed to `(String, i64)`;
  `poll()` handshake completion + language routing; `ensure_started`/`is_ready`;
  `did_open`/`did_change` return `bool` + readiness-gated; `LspManager::new` takes
  `client_version`. New manager tests for handshake + gating.
- `crates/app/src/lsp/requests.rs` — `send_request` takes a `Cap`, gates on readiness+caps;
  each `request_*` passes its `Cap`.
- `crates/app/src/app/lsp.rs` — `update_lsp` readiness gate before serialization; record
  `lsp_sent_revision` only on real send.
- `crates/app/src/app/tests/lsp.rs` — hermetic app-level assertions as needed.
- Thread `env!("CARGO_PKG_VERSION")` (editor-app) into `LspManager::new`.

## 6. Testing plan (TDD — tests first per unit)

**`editor-lsp` units (pure, CI-safe):**
- `parse_capabilities`: rust-analyzer-style full caps (utf-8 offered, providers as option
  objects, `textDocumentSync` object) → fields set, encoding = utf-8; minimal caps
  (providers as bare `true`, `textDocumentSync` as number) → fields set, sync kind decoded;
  absent providers → all false, encoding None (⇒ utf-16 default); garbage/partial → no
  panic, resilient defaults.
- `ServerCaps::allows`: truth table over every `Cap` for a caps value with a mixed
  supported/unsupported set.

**`LspManager` units (synthetic `Incoming` via `tx.send`; existing pattern):**
- Feeding `("rust", Incoming::Response { id: init_id, result: <caps> })` transitions the
  client to `Running`, stores caps, and (observably) marks ready.
- A feature request while `Initializing` returns `false` and is not correlated.
- A feature request for an unsupported capability returns `false` (gated).
- Two languages with colliding ids (both id 1) route independently via `(lang, id)`.
- Regression: existing `error_response_surfaces_as_error_event` and
  `success_response_still_parses_result` still pass with the re-keyed `pending`.

**Gates (all must stay green at each commit):** `cargo fmt --all --check`; `cargo clippy
--workspace --all-targets -- -D warnings`; `cargo test --workspace` (+ `editor-core` /
`editor-builtins` proptest suites); MSRV 1.88; no `unsafe`. App tests run with an isolated
empty `$HOME`.

## 7. Risks & edge cases

- **Init response never arrives** (hung server). PR1 has no initialize deadline (that is
  PR3's shutdown/crash lifecycle); the client stays `Initializing` and silently provides no
  features — no hang of the UI (non-blocking). Acceptable for PR1; noted for PR3.
- **Non-conformant `positionEncoding: "utf-8"`** despite utf-16-only offer → logged,
  treated as utf-16 (astral-char positions from such a server may drift; rare).
- **`initialized` ordering.** Must be sent exactly once, only after the init response,
  before any `didOpen`. The `Running` transition is the single place it is sent.
- **`lsp_sent_revision` bookkeeping** must not be set on a dropped (not-`Running`) send, or
  the doc is never opened. Guarded by the readiness gate in `update_lsp`.

## 8. Acceptance criteria

- `initialized` is sent only after `InitializeResult`; capabilities are parsed and stored.
- Every feature request is gated on the stored capability (unsupported ⇒ silent no-op).
- `initialize` sends `clientInfo`, `rootPath`, `workspaceFolders`, `trace`, and
  `general.positionEncodings`, declaring only implemented capabilities.
- Multi-server init correlates per client (no id collision).
- New unit tests cover caps parsing, the gating truth table, and the handshake/flush/gating
  transitions; all existing tests and CI gates stay green.
- Maps to the guide's **Phase-0 gate** (partial): "initialize→initialized ordering; nothing
  sent early; queued opens flushed after; capability gating on every request."
