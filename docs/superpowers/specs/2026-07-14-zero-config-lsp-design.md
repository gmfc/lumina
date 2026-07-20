# Zero-Config LSP + LSP Dock — Design

**Status:** approved decisions, pending spec review.
**Date:** 2026-07-14.
**Goal:** the editor should "just work" with language servers — open a `.rs`/`.ts`/`.py`
and the right server starts with **no config**. When a server isn't installed, the editor
**orients** the user (copy-paste install command) instead of staying silent. Server status,
progress, and logs are surfaced VSCode-style: a clickable footer indicator + a minimizable
bottom **LSP tab** sharing the dock with the terminal.

Delivered as **three PRs**, one feature each (repo norm; each is gate-green and independently
useful). PR-1 alone makes the editor "just work" for installed servers with zero UI.

---

## Decisions (locked)

| # | Decision | Choice |
|---|----------|--------|
| D1 | Discovery model | **Built-in registry + PATH probe.** A table of `lang → [candidate commands, priority order]`; first installed candidate wins. `[lsp]` config overrides a language outright. We never download servers. |
| D2 | LSP tab content | **Status rows + install help + live log tail.** |
| D3 | Status when minimized | **Clickable footer status-bar item** — spinner while initializing, `⚠N` badge on diagnostics, dim when idle; click opens the LSP tab. |
| D4 | Missing-server UX | **Auto-open the LSP tab** the first time a language resolves to "not installed," focused on that row + its install command. |
| D5 | Dock model | **Tabbed shared bottom dock** (VSCode-style): one region, a tab strip switching `Terminal | LSP`. Generalizes the existing terminal dock into a shared dock host. |
| D6 | Language scope | **Broad common set** — every language with a well-known server gets a registry entry + extension mapping. |

---

## Architecture

Three layers, matching the existing LSP + dock split:

- **`editor-lsp` / `crates/lsp`** — protocol adapter. The only change here: stop discarding server
  `stderr` and surface `window/logMessage`, so a log tail has a source.
- **`crates/app` (`lumina`)** — owns `LspManager` (process lifecycle, discovery, state) and the
  dock render/IO. The registry, PATH probing, log ring, status accessor, footer segment, and the
  dock host all live here.
- **`editor-builtins`** — the `terminal` plugin (already present) plus a thin `lsp.panel` plugin
  contributing the dock commands/keybindings (invariant #3: features are plugins, #4: everything is
  a command). Panel *content* is a pure function of app state (invariant #8) — the plugin owns
  lifecycle intent, not rendering.

No `editor-core` change — it stays headless (invariant #5). No new crate deps: PATH probing uses
`std` (`which`-style scan of `$PATH`), consistent with the no-tokio, std-only stance (§9).

---

## PR-1 — Zero-config discovery (the "just works" core)

No UI. Makes an installed server start on first open of a matching file, config-free.

### Built-in registry

New module `crates/app/src/lsp/registry.rs`:

```rust
/// One known language server: how to launch it and how to install it.
pub(crate) struct ServerDef {
    /// argv candidates in priority order; the first whose program resolves on PATH wins.
    pub candidates: &'static [&'static [&'static str]],
    /// One-line, copy-paste install hint shown in the LSP tab when none resolve.
    pub install: &'static str,
}

/// language id -> its known server. The single source of default discovery.
pub(crate) fn registry() -> &'static HashMap<&'static str, ServerDef>;
```

Seed set (D6 — every entry is a language with a widely-used server):

| lang id | ext(s) | candidates (priority) | install hint |
|---|---|---|---|
| rust | rs | `rust-analyzer` | `rustup component add rust-analyzer` |
| typescript / typescriptreact | ts / tsx | `vtsls --stdio`, `typescript-language-server --stdio` | `npm i -g @vtsls/language-server` |
| javascript / javascriptreact | js,mjs,cjs / jsx | same as typescript | `npm i -g @vtsls/language-server` |
| python | py,pyi | `pyright-langserver --stdio`, `pylsp` | `npm i -g pyright` |
| go | go | `gopls` | `go install golang.org/x/tools/gopls@latest` |
| c / cpp | c,h / cpp,cc,cxx,hpp,hxx | `clangd` | `brew install llvm` (or distro `clangd`) |
| java | java | `jdtls` | see eclipse.jdt.ls |
| kotlin | kt,kts | `kotlin-language-server` | `brew install kotlin-language-server` |
| ruby | rb | `ruby-lsp`, `solargraph stdio` | `gem install ruby-lsp` |
| php | php | `intelephense --stdio`, `phpactor language-server` | `npm i -g intelephense` |
| csharp | cs | `csharp-ls` | `dotnet tool install -g csharp-ls` |
| lua | lua | `lua-language-server` | `brew install lua-language-server` |
| bash | sh,bash,zsh | `bash-language-server start` | `npm i -g bash-language-server` |
| yaml | yaml,yml | `yaml-language-server --stdio` | `npm i -g yaml-language-server` |
| html | html,htm | `vscode-html-language-server --stdio` | `npm i -g vscode-langservers-extracted` |
| css / scss | css / scss,less | `vscode-css-language-server --stdio` | `npm i -g vscode-langservers-extracted` |
| json | json,jsonc | `vscode-json-language-server --stdio` | `npm i -g vscode-langservers-extracted` |
| toml | toml | `taplo lsp stdio` | `cargo install taplo-cli --features lsp` |
| sql | sql | `sqls` | `go install github.com/sqls-server/sqls@latest` |
| swift | swift | `sourcekit-lsp` | ships with Xcode / Swift toolchain |
| zig | zig | `zls` | see zigtools/zls |
| markdown | md,markdown | `marksman` | `brew install marksman` |

(Table is data; adding rows later is a one-line change and needs no code.)

### Discovery flow

- **`files.rs::language_for`** — extend the extension→lang map to cover the table above (adds the
  flagged `tsx`→typescriptreact, `jsx`→javascriptreact, `cpp/cc/…`, `yaml`, `html`, `css/scss`,
  `java`, `rb`, `sh`, `lua`, `sql`, `swift`, `zig`, `kt`, `php`, `cs`, `pyi`, `jsonc`). This is the
  single source of `lang` feeding the manager, so ids must match registry keys exactly.
- **`LspManager` value type** changes from a single resolved `Vec<String>` to a *resolution*: for a
  language, walk `[config override] ∪ [registry candidates]`, probe each program against `$PATH`
  (cached), and record one of `Resolved(argv)` / `NotInstalled(install_hint)`. Config (`[lsp]`)
  wins: an explicit `lang = "…"` replaces the candidate list for that language.
- **`ensure_started`** spawns the `Resolved(argv)`; for `NotInstalled` it does **not** spawn and
  does **not** retry every tick (records the state once). Unknown languages (no registry entry, no
  config) stay inert.
- **`is_enabled()`** becomes per-language aware: the editor is "LSP-capable" if any open language
  resolves to a server. Gate features on `is_ready(lang)` as today.

### Upward root search

New helper (in `files.rs` or `lsp/registry.rs`): from the opened file, walk parents for the nearest
`Cargo.toml` / `package.json` / `go.mod` / `pyproject.toml` / `.git`, and use that dir as
`root_uri`. Falls back to the launch root. Fixes rust-analyzer breaking when a file is opened deep in
a tree. Applied where `root_uri` is computed (`LspManager::new` / `app/lifecycle.rs`).

### PR-1 tests

Registry/probe unit tests (a fake PATH dir, assert first-candidate-wins + config-override +
not-installed); `language_for` table tests incl. tsx/jsx; upward-root-search tests (temp dir tree);
one end-to-end mock-server test proving a zero-config `rust` open spawns + handshakes (reuse the
`mock_lsp_server` harness + the `update_lsp` App test pattern).

---

## PR-2 — Server state model + footer indicator

Plumbing + the always-visible status item. Testable via the mock server; visible value (spinner +
badge) even before the panel lands.

### Capture logs (the missing source)

- `crates/lsp/src/client.rs:59` — `Stdio::null()` → `Stdio::piped()`; take `child.stderr`; spawn a
  reader thread (mirror the stdout reader) that forwards lines as a new `ClientMsg::Stderr(String)`.
- `crates/app/src/lsp/server_msgs.rs` — add a `window/logMessage` branch (currently dropped) → same
  log sink.

### New `LspManager` state (all in `crates/app/src/lsp.rs`)

- `logs: HashMap<String, VecDeque<String>>` — bounded per-language ring (e.g. cap 500 lines), fed by
  `ClientMsg::Stderr` + `logMessage`.
- `last_error: HashMap<String, String>` — written at the three `LspEvent::Error` push sites.
- `diag_counts: HashMap<String, (usize, usize)>` — (errors, warnings) per language, updated in
  `on_diagnostics` / `handle_pull_report`, cleared on close/crash.
- Read-only accessor:

```rust
pub(crate) struct LangStatus {
    pub lang: String,
    pub state: LangState,          // Initializing | Running | NotInstalled | Crashed | Backoff
    pub command: Option<String>,   // resolved argv joined, or None
    pub install_hint: Option<String>,
    pub progress: Option<String>,  // e.g. "Indexing 342/1200" (from ProgressItem)
    pub errors: usize,
    pub warnings: usize,
    pub last_error: Option<String>,
}
pub(crate) fn status_rows(&self) -> Vec<LangStatus>;
pub(crate) fn status_summary(&self) -> LspSummary;   // worst state + total error/warn counts
```

### Footer status-bar item

- `render_status` (`ui/chrome.rs`) — append a right-aligned `LSP` segment before the `Ln/Col`
  cluster: spinner (reuse `spinner_frame()`) while any server initializes / indexes; `⚠N`/`✗N` from
  `status_summary` diag counts; dim glyph when idle-and-clean. Colors reuse existing
  `lsp.diag.error/warning` theme inks.
- Publish the summary app-side each `update_lsp` tick into `editor.status_items["lsp.status"]` (the
  established mirror pattern, cf. `lsp.progress`) so `chrome` needn't reach into the private
  `App.lsp` field.
- Make it clickable: new `Regions.lsp_status: Option<Rect>` recorded in `draw()`; a new arm in
  `mouse_left_down` → `exec_id("dock.focusLsp")` (the command lands in PR-3; until then it toggles a
  no-op — sequence PR-3 before shipping the click, or land the command stubbed).

### PR-2 tests

Mock-server test asserting stderr + `logMessage` land in `logs`; `status_rows`/`status_summary`
state-machine tests (initializing→running→crashed→backoff); a render test asserting the footer shows
the spinner while initializing and `⚠N` with diagnostics.

---

## PR-3 — Tabbed shared dock + LSP panel + auto-open

Turns the terminal dock into a shared, tabbed bottom dock and adds the LSP tab.

### Generalize the dock (D5)

- **Dock chrome moves to app/EditorState**: a `DockView { open: bool, minimized: bool, active: DockTab }`
  where `DockTab ∈ { Terminal, Lsp }`; `dock_height: u16` (from config `terminal_height`, kept as
  the dock height). The terminal plugin keeps owning its *sessions* (`TerminalView` = order/active
  terminal) but no longer owns open/minimized — those become dock-level.
- **Focus**: add `Focus::Dock` replacing the terminal-specific `Focus::Panel` semantics; key/mouse
  routing dispatches to the active tab (terminal keys only when `active == Terminal`).
- **Commands** (invariant #4): `dock.toggle`, `dock.minimize`, `dock.focusTerminal`,
  `dock.focusLsp`, `dock.nextTab`. `terminal.toggle` becomes "open dock + focus Terminal"
  (back-compat keybinding `Ctrl+J`); LSP gets `Ctrl+Shift+L` → `dock.focusLsp`.
- **Header**: the panel header grows a top-level tab strip `[ Terminal ][ LSP ]` + minimize chevron;
  terminal's per-session tabs render below/within the Terminal tab only.

### LSP panel content (D2)

- `render_lsp_panel(f, app, area)` (pure over `LspManager::status_rows()` + `logs`, invariant #8):
  a status table (one row per language: state glyph, lang, resolved command or `(not installed)`,
  progress text, error/warn counts) above a scrollable log tail for the selected row.
- Interactions: click a `not installed` row → copy its install command to the clipboard + status
  message; a `restart` affordance on a crashed row → `exec_id`/manager restart; wheel scrolls the log
  region.
- Thin `lsp.panel` builtin plugin contributes the `dock.focusLsp` command + keybinding (invariant
  #3); content reads app state.

### Auto-open (D4)

The first time `ensure_started` resolves a language to `NotInstalled`, open the dock focused on the
LSP tab + that row (once per language — a `HashSet<lang>` guard so it never nags twice).

### PR-3 tests

Dock state-machine tests (toggle/minimize/tab-switch/focus routing terminal-vs-lsp);
`render_lsp_panel` render tests (installed row, not-installed row shows install cmd, crashed row);
auto-open-once test (missing server opens the tab exactly once per language); regression tests that
the terminal still works through the generalized dock.

---

## Risks / notes

- **PR-3 touches the just-merged terminal dock.** Mitigated by keeping session ownership in the
  terminal plugin and moving only chrome (open/minimized/active-tab) up; terminal regression tests
  guard behavior.
- **PATH probing cost** — cache resolution per language (probe once, not per tick).
- **Server arg drift** — registry argv are best-effort defaults; `[lsp]` override remains the escape
  hatch and always wins, so a wrong default is user-fixable without a code change.
- **`is_enabled` semantics change** — audit call sites (currently `!servers.is_empty()`), which
  gates the `lsp` plugin's no-op path.

## Non-goals

Downloading/managing server binaries; per-project server config UI beyond `[lsp]`; multi-root
workspaces; a full Problems panel with navigation (the caret-diagnostic status line already covers
jump-to-diagnostic).
