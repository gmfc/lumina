# LSP PR4 — Cancellation, staleness guards, error matrix

**Date:** 2026-07-13
**Status:** Approved (roadmap PR4; proceed-to-completion)
**Crates:** `editor-lsp`, `editor-app`.

## Problem (guide §1.4, §9.4, §9.5)

- **No cancellation.** Superseded requests (a new keystroke's completion, a moved cursor's
  hover) are never `$/cancelRequest`'d, so slow servers keep computing dead work and the
  editor feels laggy.
- **No staleness guard.** A response is applied by request-kind alone, so a stale answer
  (buffer moved since the request, or an A/B/A race) renders/applies at shifted positions.
- **No error matrix.** Every JSON-RPC error surfaces to the user identically —
  `-32800`/`-32801`/`-32802` (cancellation/staleness/load-shed, not user errors) look the
  same as `-32803 RequestFailed`. The error *code* was discarded entirely.

## Design

**Crate (`editor-lsp`):**
- `ResponseError { code, message }` replaces `Incoming::Response.error: Option<String>`;
  `classify` preserves the code. Well-known code constants + `is_droppable()`
  (`REQUEST_CANCELLED`/`CONTENT_MODIFIED`/`SERVER_CANCELLED` → true).
- `LspHandle::cancel(id)` sends `$/cancelRequest { id }` (advisory; the pending entry stays
  until the server's response arrives, §1.4).

**Manager (`crates/app/src/lsp.rs`):**
- `PendingEntry { kind, uri, version }` replaces the bare `Pending` in the pending map — every
  request is tagged with the buffer version it was asked against (from `versions[uri]`, which
  is current thanks to edits-before-dependents, §1.7).
- `inflight: HashMap<(lang, Pending), i64>` tracks the latest in-flight id per **cancelable**
  kind (`is_cancelable`: Hover, Definition, Completion — passive lookups; rename/references/
  symbols run to completion, §9.3). Issuing a new cancelable request `cancel`s the prior one.
- `poll()` response handling:
  - **Superseded** (a newer id is in flight for this kind) → drop.
  - **Error matrix** (§9.5): `!superseded && !err.is_droppable()` → surface `Error(message)`;
    otherwise silent.
  - **Stale by version** (`versions[uri] != entry.version`) → drop (edit-producing results
    never apply across versions; display results just drop).
- `handle_exit` also clears `inflight` for the dead language.

The monotonic version guard subsumes the §9.4 generation counter (an A/B/A edit still lands
on a different version number), so no separate generation is needed while there are no
decoration features to position-shift.

## Non-goals (later)
Retry-on-`-32802`/`-32801` (we drop; the user retriggers, or the newer request's response
arrives); per-feature debounce timers (§9.3) — those attach when the UI features need them;
decoration position-shifting.

## Testing
- Crate: `is_droppable` matrix; `classify` preserves the code.
- Manager: stale-by-version drop; superseded cancelable drop (older id dropped, newer wins);
  `-32801` dropped silently; (`-32603` surfaced — existing test).
- Real clangd: a cancelled completion elicits `-32800` and the connection survives (verified).
- Gates green.
