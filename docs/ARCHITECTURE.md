# Lumina Architecture Guide

A practical, opinionated reference for how Lumina is built and *why* the idioms
are what they are. This is the guide the doc comments mean when they cite
"CLAUDE.md invariant #N" and "plan §N" — the invariants and the layering live
here now, in one place.

It is written against a general [Rust application & system architecture
philosophy](#appendix-the-general-rust-philosophy) and then made concrete for
this editor. Where the general advice doesn't fit a terminal editor (async
runtimes, distributed tracing), we say so rather than cargo-culting it.

---

## Table of Contents

1. [The Invariants](#1-the-invariants)
2. [Workspace & Dependency Direction](#2-workspace--dependency-direction)
3. [Module Organization](#3-module-organization)
4. [Type-Driven Design](#4-type-driven-design)
5. [Error Handling](#5-error-handling)
6. [Ownership & API Design](#6-ownership--api-design)
7. [Ports & Adapters: the plugin kernel](#7-ports--adapters-the-plugin-kernel)
8. [Architectural Patterns in Use](#8-architectural-patterns-in-use)
9. [State & Concurrency](#9-state--concurrency)
10. [Configuration](#10-configuration)
11. [Testing Strategy](#11-testing-strategy)
12. [Build, CI & Lints](#12-build-ci--lints)
13. [Anti-Patterns We Avoid](#13-anti-patterns-we-avoid)
14. [Appendix: the general Rust philosophy](#appendix-the-general-rust-philosophy)

---

## 1. The Invariants

These are load-bearing. Doc comments across the tree reference them by number;
a change that breaks one is a change to the architecture, not a detail. Keep the
numbering stable — code cites it.

| # | Invariant | Enforced by |
|---|---|---|
| **1** | **All buffer mutation goes through a reversible `Transaction`.** Nothing outside `editor-core` touches the rope directly; the raw `apply_raw_*` helpers are private to `core`. This is what makes undo/redo total and multi-cursor edits atomic. | `Transaction` is the only public mutation path; `Host::apply_transaction` is the sole edit entry for plugins. |
| **2** | **A document holds a *set* of selections, not one cursor.** Multi-cursor is the default shape, not a bolted-on mode. | `Selections` is a normalized (sorted, non-overlapping) set; every edit op maps over it. |
| **3** | **Features are plugins.** Built-ins (the explorer) register through the *same* contribution API as third-party plugins — no privileged back doors. | `editor-builtins` reaches the editor only through `editor_plugin::Host`; the self-hosting test proves it. |
| **4** | **Everything is a command.** Every user-visible action is a named command with an id, invokable from the palette, a keybinding, or another command. | `CommandSpec { id, title }` + `Host::execute(id)` for composition. |
| **5** | **`editor-core` stays headless.** Pure editing logic (auto-pairs, motions, screen↔buffer column math) lives in library crates with zero terminal/UI deps. | `core` depends only on rope/unicode/slotmap; no `crossterm`/`ratatui`. |
| **6** | **Coordinate mapping is a pure function; file fidelity is preserved.** `screen_to_char`/`char_to_screen` are pure; a file's detected encoding and line ending are re-emitted verbatim on save, never silently rewritten. | Pure functions in `core`; `files.rs` records and re-emits the original line ending. |
| **7** | **The core is unit-testable without a TTY.** The consequence of #5: the domain runs in milliseconds under `cargo test` with no terminal. | `editor-core` has no I/O; tests construct documents directly. |
| **8** | **Rendering is a pure function of state.** `render(state) -> frame` mutates nothing — the editor view and the terminal panel alike. | Render paths take `&State`; all mutation happens in command/event handling. |
| **9** | **Saves are atomic.** Write to a temp file + rename, so a crash or an external reader never observes a partial write. | `files.rs` temp-write-then-rename. |

The one-line creed (from the README): *everything is a command; a document holds
a set of selections; features are plugins; render is a pure function of state;
all buffer mutation goes through the transaction API.*

---

## 2. Workspace & Dependency Direction

Lumina is a six-crate workspace. The split is the Helix/VS Code "headless core,
thin view" seam, and the crate graph *enforces* the dependency direction — a
lower crate literally cannot `use` a higher one because it isn't in its
`Cargo.toml`.

```
              editor-app          ← the lmn binary: event loop, ratatui, keymap, wiring
             /    |     \
   editor-builtins |   editor-lsp / editor-syntax   ← adapters
             \    |     /
           editor-plugin           ← the kernel: contribution API, Host port, registry, runtimes
                  |
             editor-core           ← the domain: rope, selections, transactions, motions (no I/O)
```

| Crate | Role | Depends on |
|---|---|---|
| `editor-core` | Headless model: rope, normalized multi-cursor `Selections`, reversible `Transaction`/undo, motions, pure `screen_to_char`/`char_to_screen`. **No terminal deps.** | ropey, unicode, slotmap |
| `editor-syntax` | tree-sitter parse + highlight-query → capture spans, cached, viewport-only. UI-free. | core, tree-sitter |
| `editor-lsp` | LSP client: JSON-RPC framing, UTF-16 position conversion, diagnostics. | serde, lsp-types |
| `editor-plugin` | The contribution API (traits + registries + event bus), the `Host` surface, and the external plugin runtimes. **The kernel.** | core, rhai, wasmi |
| `editor-builtins` | Core features implemented **as plugins** (the explorer). | core, plugin |
| `editor-app` | The `lmn` binary: event loop, ratatui rendering, keymap, wiring. | all of the above |

**Why the split earns its keep here** (not just to have small crates):

- **Dependency inversion is compiler-enforced.** `core` cannot depend on
  `ratatui` because it isn't listed — invariant #5 can't be violated by accident.
- **Incremental compilation.** Editing the render layer in `app` doesn't
  recompile `core`.
- **Shared versions.** All third-party versions live in
  `[workspace.dependencies]`; members write `foo.workspace = true` so versions
  never drift. Internal path deps (`editor-core = { path = ... }`) are declared
  once at the root too.

Rule of thumb before adding a crate: is there a real dependency-direction rule
or reuse boundary to enforce? If not, a module is the right granularity (§3).

---

## 3. Module Organization

- **`foo.rs` + `foo/` over `mod.rs`.** The tree already migrated to the 2018
  style (`document.rs` beside `document/`, `app.rs` beside `app/`, `edit.rs`
  beside `edit/`). Keep it — the module's own code stays visible instead of
  buried in a wall of identically-named `mod.rs` tabs.
- **`pub(crate)` / `pub(super)` are the workhorses.** `core`'s raw rope helpers
  are `pub(super)` precisely so invariant #1 holds. Reserve bare `pub` for the
  actual cross-crate surface.
- **Curated facades.** Each `lib.rs` `pub use`s the handful of types consumers
  need and keeps internal module paths free to move.
- **Organize by feature, not by technical layer.** `core` groups `edit/`,
  `motion/`, `document/`, `selection` — cohesive units — rather than a
  `models/` + `helpers/` dumping ground.

Every crate opens with a module doc comment stating its job and which invariants
it upholds. Keep that habit: it's how a reader knows the contract before the code.

---

## 4. Type-Driven Design

This is where the editor's correctness lives.

- **Newtype keys.** `DocId` is a `slotmap` `new_key_type!` — you cannot pass a
  tab index where a document handle belongs, and stale handles don't silently
  alias a recycled slot.
- **Enums model state.** Panel locations, contribution kinds, and edit ops are
  enums whose variants carry exactly their own data, so `match` forces every
  call site to handle every case — add a variant and the compiler lists the work.
- **`Selections` is correct by construction.** It is normalized (sorted,
  non-overlapping) at the boundary; downstream edit code never re-checks the
  invariant. That's "parse, don't validate" applied to the cursor set.
- **`Transaction` makes illegal edits unrepresentable.** Because it's the only
  public mutation and it records its own inverse, "a change that can't be undone"
  is not a state the type system lets you build (invariant #1).

Reserve `panic!`/`unwrap`/`expect` for genuine invariant violations, never for
bad input or recoverable I/O.

---

## 5. Error Handling

The split follows the library-vs-binary rule, tuned to Lumina's reality:

- **Library crates return `std::io::Result` / narrow `Result`s.** `editor-lsp`
  returns `io::Result<T>` from transport and request methods — the failure modes
  are genuinely I/O, and the caller (the app) reacts by logging/degrading, not by
  matching a bespoke taxonomy. This is deliberately *not* a `thiserror` enum: a
  hand-rolled 40-variant error nobody matches on would be ceremony, not safety.
  Introduce a typed `thiserror` enum the moment a caller needs to branch on a
  specific, stable failure — that's the trigger, not "it's a library."
- **The binary uses `anyhow`.** `editor-app` (`app.rs`, `files.rs`, `main.rs`)
  uses `anyhow::Result` with `.context()` so a failure to load config or save a
  file produces a diagnosable chain, and `main` prints it on exit.
- **Convert at the boundary.** The app catches library `io::Result`s, adds
  context, and lets them bubble as `anyhow::Error`.

Rules, non-negotiable:

- Never swallow errors silently. If you truly ignore one, log at `debug`/`warn`
  and say why.
- Never `unwrap()` on I/O or parsing in a production path.
- Atomic save (invariant #9) exists because a half-written file *is* an
  unrecoverable error — design the operation so the failure can't corrupt state.

---

## 6. Ownership & API Design

Signatures encode intent, and Lumina's already lean the right way:

- **Borrow the general form.** Take `&str`/`&[T]`/`&Path`, not `&String`/`&Vec`.
- **`impl Into<String>` in constructors that store.** `CommandSpec::new(id: impl
  Into<String>, title: impl Into<String>)` is the canonical example — ergonomic
  for callers, honest about the ownership it takes.
- **Own what you store; don't take `&T` and clone inside.** Make the caller's
  cost visible.
- **Owned application state, not lifetime-threaded structs.** Long-lived state
  (`Workspace`, documents) owns its data; borrowed structs are reserved for
  transient views (iterators, parsers, the highlight pass over a viewport slice).

---

## 7. Ports & Adapters: the plugin kernel

This is Lumina's spine and the clearest instance of dependency inversion in the
tree.

`editor-plugin` defines two traits that are the ports:

```rust
// The imperative surface a plugin drives. Implemented by the app's editor state;
// the same object is passed to native and (via a marshaling shim) external plugins.
pub trait Host {
    fn workspace(&self) -> &Workspace;
    fn apply_transaction(&mut self, doc: DocId, txn: Transaction);   // invariant #1
    fn set_selections(&mut self, doc: DocId, selections: Selections);
    fn open_path(&mut self, path: &Path);
    fn read_dir(&self, path: &Path) -> Vec<DirEntry>;               // capability-gated
    fn set_panel(&mut self, panel_id: &str, content: PanelContent);
    fn notify(&mut self, message: String);
    fn execute(&mut self, command_id: &str);                        // command composition (#4)
}

pub trait Plugin { /* declares Contributions, handles events */ }
```

- **The app implements `Host`.** Business features (`editor-builtins`) depend on
  the `Host` *trait*, never on the app. That's why the explorer is a plugin and
  not a hardcoded panel (invariant #3).
- **`read_dir` is the capability seam.** Plugins don't touch `std::fs`; they ask
  the host, which enforces `fs:read` grants for external guests. Deny-by-default.
- **Two substrates, one API.** Rhai scripts and WebAssembly guests both drive the
  same `Registry`/`Host`. WASM guests run with **no host imports**, fuel-metered
  against runaway loops (plan §11) on `wasmi`. There is no privileged path — the
  self-hosting test asserts that disabling a plugin removes exactly its
  contributions and nothing else.

Static vs dynamic dispatch: the registry stores `Vec<Box<dyn Plugin>>` (a
heterogeneous collection — dynamic dispatch is the correct tool there). Hot,
single-implementation paths in `core` stay generic/monomorphized.

---

## 8. Architectural Patterns in Use

Lumina composes a few patterns *inside* the hexagonal skeleton rather than
picking one dogmatically:

- **Hexagonal / ports & adapters** — the outer skeleton (§2, §7). Domain in the
  center, adapters (LSP, syntax, terminal, filesystem) at the edges, dependencies
  pointing inward and enforced by the crate graph.
- **Command pattern** — every action is a `CommandSpec` (#4), and every buffer
  change is a reversible `Transaction` (#1). This is exactly the
  apply/invert-on-an-undo-stack shape the general guide recommends for editors,
  and it's what makes undo/redo, macros, and future collaborative editing
  tractable.
- **Message passing over shared locks** — see §9. The event loop and the
  PTY-backed terminal panel communicate over `mpsc` channels; there is almost no
  shared-mutable state to lock.

We deliberately do *not* reach for ECS (no many-entity simulation loop) or an
async actor framework (see §9).

---

## 9. State & Concurrency

Lumina is **synchronous and single-owner by design.** There is no `tokio`, no
async runtime coloring the codebase — a terminal editor is an event loop over
one user's input, not a server juggling thousands of connections. This is a
feature: the whole domain is trivially testable (invariant #7) and there are no
executor hazards.

Concrete consequences, and they line up with the general guide's state decision
tree landing on its *first* answer ("can one owner hold it? then just own it"):

- **Almost no shared-mutable state.** The tree has a single `Mutex` and no
  `Arc<Mutex<T>>` sprawl. State has one owner; mutation goes through `&mut`.
- **Concurrency is message passing.** Background work (the PTY reader for the
  terminal panel, the filesystem watcher for live file-sync) runs on OS threads
  and talks to the event loop over `std::sync::mpsc` channels — "share memory by
  communicating." The main loop owns editor state exclusively; workers send it
  messages.
- **No locks held across boundaries**, because there are effectively no shared
  locks to hold. If you ever add one, keep the critical section tiny and never
  span it across a blocking wait.

If a future feature genuinely needs shared read-mostly state, `Arc<T>` (clone the
handle, no lock) is the first tool; a channel/owning-thread is preferred over
`Arc<Mutex<T>>` for shared *mutation*. Reach for a lock only when message passing
is genuinely more complex, and justify it.

---

## 10. Configuration

- **Parsed into a typed struct once, at startup.** `~/.config/lumina/config.toml`
  deserializes via `serde` into a typed `Config` (`[settings]`, `[keys]`,
  `[lsp]`, `[theme]`). The rest of the app receives a guaranteed-valid value and
  never re-parses.
- **Sensible defaults, overridable.** Settings default so a missing file still
  boots; keys merge onto the default keymap; `LMN_INSTALL_DIR`/`LMN_VERSION`
  parameterize the installers.
- **Runtime, not build-time.** One binary runs everywhere; deployment knobs
  (shell, poll-watch for NFS/devcontainers, gutter, icons) are config, not
  `cfg!`.

---

## 11. Testing Strategy

The payoff of the headless core (#5, #7) is that most tests need no terminal.

- **Unit tests** (`#[cfg(test)] mod tests`) are the bulk and run in
  milliseconds — the domain has no I/O to mock.
- **Property tests** live behind each crate's `proptests` feature so the
  workspace stays green from commit one, and CI activates each suite at the phase
  where its subject type exists (transaction round-trip, selection invariants,
  coordinate mapping, self-hosting). This is `proptest` finding the edge cases a
  hand-written table never would — exactly right for a transaction/undo model and
  a coordinate mapper with algebraic invariants (`decode ∘ encode == id`).
- **Test doubles via traits, not mock frameworks.** Because `Host` is a trait
  (§7), testing a plugin needs a plain in-memory `Host`, not a mocking DSL.
- **The self-hosting test is the architecture's guardrail.** It proves built-ins
  reach the editor only through the contribution API (invariant #3) by asserting
  that disabling a plugin removes exactly its contributions.
- **Add a regression test with every bug fix.** That's how the bug stays dead.

---

## 12. Build, CI & Lints

CI (`.github/workflows/ci.yml`) gates every PR on:

```bash
cargo fmt --all --check                                  # formatting is not a debate
cargo clippy --workspace --all-targets -- -D warnings    # lints as errors
cargo test --workspace                                   # ubuntu + macos + windows
cargo build --workspace                                  # on the 1.88 MSRV floor
```

**Lints are centralized** in the root `Cargo.toml`, the single source of truth
the general guide recommends; every member opts in with `lints.workspace = true`:

```toml
[workspace.lints.rust]
unsafe_code = "forbid"     # stronger than the guide's "warn" — Lumina has zero unsafe

[workspace.lints.clippy]
all = "warn"               # promoted to deny by CI's -D warnings
```

`unsafe_code = "forbid"` replaces the six per-crate `#![forbid(unsafe_code)]`
attributes with one workspace rule — the whole tree is unsafe-free and stays that
way by construction. Treat Clippy as a senior reviewer: fix it, or `#[allow]`
with a justifying comment; never mute it silently.

MSRV is pinned at `rust-version = "1.88"` (the dependency floor, via the
`darling` proc-macro used through serde/rhai derives) and tested in CI. `Cargo.lock`
is committed for reproducible builds. A separate SonarQube workflow tracks
coverage.

---

## 13. Anti-Patterns We Avoid

- ☑ **No `Arc<Mutex<T>>` sprawl** — one `Mutex` in the whole tree; state has an
  owner and workers use channels (§9).
- ☑ **No `.clone()` to dodge the borrow checker** — signatures borrow the general
  form and own what they store (§6).
- ☑ **No `unsafe`** — forbidden workspace-wide (§12).
- ☑ **No stringly-typed handles** — `DocId` is a newtype key (§4).
- ☑ **No business logic importing the view** — `core` can't see `ratatui`; the
  crate graph forbids it (§2, invariant #5).
- ☑ **No god trait** — `Host` is a focused imperative surface; contributions are
  declarative specs, split by kind.
- ☑ **No raw rope mutation** — everything goes through `Transaction` (#1).

Watch for these as the tree grows: over-`pub` surfaces, lifetimes threaded through
application state, premature crate-splitting, and any blocking work that would
starve the input loop (route it to a worker thread + channel, per §9).

---

## Appendix: the general Rust philosophy

The house style Lumina is an instance of. Internalize these; the sections above
are them, applied.

- **Make illegal states unrepresentable.** Every invariant encoded in a type is a
  bug class that can't ship. Prefer an `enum` over a `bool` + comment; a newtype
  over a bare `String`.
- **Parse, don't validate.** Convert unstructured input into a structured type
  *once*, at the boundary (`Selections`, `Config`, `Email::parse`). Downstream
  code never re-checks.
- **Push side effects to the edges.** A pure, deterministic core; I/O, clocks, and
  randomness in a thin shell. This is what makes the domain testable without
  mocking (#5, #7).
- **Ownership models responsibility.** Design ownership deliberately; don't reach
  for `Rc<RefCell<T>>` or `Arc<Mutex<T>>` to escape a design you haven't finished.
- **Compile-time over run-time.** Favor static dispatch and the type system;
  pay for `dyn` only where you need runtime polymorphism (the plugin registry).
- **The borrow checker is a design tool.** When it fights you, it's usually
  pointing at a real ownership ambiguity — restructure rather than reach for
  `unsafe` or reference counting.
- **Errors are values.** Typed and matchable in libraries the moment a caller
  branches on them; contextual (`anyhow`) in binaries. Preserve the source chain;
  never swallow silently.
- **Measure before optimizing.** Reserve capacity when the size is known, avoid
  needless clones, prefer iterators over intermediate `Vec`s — but profile
  (`criterion`, a flamegraph) before trading clarity for speed.

*The compiler is on your side. Design so it can help you.*
