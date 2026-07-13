# LSP PR3 — Lifecycle robustness (shutdown, crash/restart/resync, didClose)

**Date:** 2026-07-13
**Status:** Approved (roadmap PR3; proceed-to-completion)
**Crates:** `editor-lsp`, `editor-app`.

## Problem (guide §3.8/§3.9, §4.1)

- **Shutdown** only fires `shutdown` + `exit` with no ordered wait/deadline (`Drop` then
  `kill`s immediately).
- **Crash** is invisible: when a server exits unexpectedly the reader thread ends silently,
  the stale handle stays in `clients`, all later I/O fails quietly, pending requests **hang
  forever**, its diagnostics linger, and there is no restart.
- **`didClose` is never sent** — closing a tab leaks the document in the server's mirror.

## Design

### A. Detect connection death via the merged channel
The per-connection forwarding thread already ends when the server's stdout hits EOF. Make it
**signal** that: the merged channel carries `ClientMsg` instead of bare `Incoming`:

```rust
enum ClientMsg { Msg(Incoming), Exited }   // in crates/app/src/lsp.rs
// channel: Sender<(String, ClientMsg)>
```
The forwarding thread sends `Msg(i)` per inbound message, then one `Exited` when `rx.recv()`
returns `Err` (server gone). `poll()` handles `Exited` → the crash path (§C).

### B. Ordered shutdown with deadline (§3.8)
`LspHandle::stop(deadline)`: send `shutdown` request, send `exit` notification, then wait for
the child up to `deadline` (poll `child.try_wait()` in a short sleep loop), then `kill`. Keep
`Drop` as a last-resort `kill` + `wait` (never blocks quit beyond the deadline). Called on
`:lsp-restart`, config reload, and editor quit. Timeout = 3 s (§9.6).

### C. Crash handling + restart with circuit breaker (§3.9)
On `Exited` (or any send-while-dead), for that language:
1. **Fail pending locally**: drain `pending` entries for that language → emit `LspEvent::Error`
   ("server exited") for edit-producing kinds; drop the rest. Clear its `state`, `versions`
   bookkeeping, and remove the handle.
2. **Clear server-pushed UI**: emit `LspEvent::Diagnostics` with an empty set for that
   server's docs so stale squiggles clear. (Track which URIs the server published for.)
3. **Restart policy** *(≈ VS Code)*: record crash `Instant`s per language; if **5 within 3
   minutes** → mark `Crashed`, one statusline error, no auto-restart. Else re-`ensure_started`
   after exponential backoff (250 ms → 4 s). Backoff is enforced by a "not before" `Instant`
   checked in `ensure_started`.
4. **Resync** on the *next* successful handshake: the app's `update_lsp` re-sends `didOpen`
   for every attached doc because `forget`/crash cleared `lsp_sent_revision` for them.
   Versions stay **monotonic per document** — the `versions` counter is not reset (guide
   §3.9); a restarted server gets `didOpen` at the continuing version.

Because `poll()` runs on the UI thread, restart timing uses `std::time::Instant` (allowed in
app code; only workflow scripts forbid `Date::now`).

### D. `didClose` (§4.1)
Hook `crates/app/src/app/file_ops.rs::close_and_forget`: before `workspace.close_tab(idx)`
removes the doc, capture its path + language and call `self.lsp.did_close(path, lang)` (sends
only if the connection is `Running`). `forget_doc` already clears `lsp_sent_revision`.

## Non-goals (later)
`:lsp-restart` command UI (wire the mechanism; command is trivial follow-up); per-URI publish
tracking beyond what's needed to clear on crash; multi-root; workspace/didChangeConfiguration
on resync (no config yet). Progress UI stays PR-later.

## Testing
- Crate: `stop` sends shutdown-then-exit ordering (extract a pure frame-sequence check where
  possible); `did_close` frame shape.
- Manager (synthetic `(lang, ClientMsg::Exited)`): pending for that language are failed
  (errors emitted), state cleared, handle removed; circuit breaker trips after 5 crashes in a
  simulated 3-min window (inject `Instant`s via a seam); backoff gate blocks immediate
  restart. Diagnostics-clear event emitted on crash.
- App: closing a tab attempts `did_close` (no-op without a server; assert no panic + state
  forgotten). Resync: after a simulated restart, `update_lsp` re-sends `didOpen`.
- Gates green; regression vs clangd (clean shutdown, exit 0).

## Risks
- Crash-restart storms: the 5-in-3-min breaker + backoff bound it.
- Injecting time for the breaker test without leaking a clock everywhere: pass an
  `Instant`-returning closure or a small `now: fn() -> Instant` seam to the breaker helper, or
  test the pure breaker predicate `should_restart(&[Instant], now)` directly.
