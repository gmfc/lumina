# lumina

A mouse-first, VS Code-like **terminal** code editor in Rust, built on its own plugin
system. Tabs, a clickable directory explorer, full mouse support, syntax highlighting,
find/replace, project search, live file-sync, multi-cursor, an LSP client, an integrated
terminal panel, and a sandboxed external plugin runtime.

## Screenshots

![lumina editing its own source — syntax highlighting, tabbed editing, and a clickable file explorer](docs/lumina-editor.webp)

![lumina's start screen with quick key hints](docs/lumina-welcome.webp)

## Architecture

Six crates (headless core, thin view — the Helix/VS Code split):

| Crate | Role |
|---|---|
| `editor-core` | Headless model: rope, normalized multi-cursor selections, reversible transactions/undo, motions, and the pure `screen_to_char`/`char_to_screen` coordinate mapping. No terminal deps. |
| `editor-syntax` | tree-sitter parsing + highlight-query → capture spans (cached, viewport-only). |
| `editor-lsp` | LSP client: JSON-RPC transport, UTF-16 position conversion, diagnostics. |
| `editor-plugin` | The contribution API (traits + registries + event bus), the `Host` surface, and the external plugin runtime — the kernel that hosts plugins. |
| `editor-builtins` | The core features implemented **as plugins** (the explorer). |
| `editor-app` | The `lumina` binary: event loop, ratatui rendering, keymap, and wiring. |

Everything is a command; a document holds a *set* of selections; features are plugins;
render is a pure function of state; all buffer mutation goes through the transaction API.

## Install

Prebuilt `lmn` binaries are published for macOS, Windows and Linux on every tagged
release. Once installed, open a directory just like vim:

```sh
lmn .              # open the current directory
lmn src/main.rs    # open a single file
```

**macOS / Linux** (installs to `~/.local/bin`):

```sh
curl -fsSL https://raw.githubusercontent.com/gmfc/lumina/main/install.sh | sh
```

**Windows** (PowerShell):

```powershell
irm https://raw.githubusercontent.com/gmfc/lumina/main/install.ps1 | iex
```

Override the destination with `LMN_INSTALL_DIR`, or pin a version with
`LMN_VERSION=v0.1.0`. Supported targets: `x86_64`/`aarch64` Linux, Intel/Apple-silicon
macOS, and `x86_64` Windows.

**From source** (any platform with Rust ≥ 1.88):

```sh
cargo install --git https://github.com/gmfc/lumina editor-app   # installs `lmn`
```

**Updating** — pull the newest release in place (safe to run while the editor is open):

```sh
lmn update        # re-runs the installer for your OS, upgrading this binary
lmn --version     # check what you're on
```

Re-running the install one-liner above does the same thing. The installers replace the
binary atomically, so a running instance keeps working until you restart it.

## Build & run

```sh
cargo build --workspace
cargo test  --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
cargo run -p editor-app -- <path>     # or: cargo run --bin lmn -- <path>
```

## Keys (defaults, remappable in config)

`Ctrl+P` quick-open · `Ctrl+Shift+P` command palette · `Ctrl+F`/`Ctrl+H` find/replace ·
`Ctrl+Shift+F` project search · `Ctrl+B` toggle sidebar · `Ctrl+D` add cursor at next match ·
`Ctrl+F2` select all occurrences · `Shift+Alt+I` cursors to line ends · `Alt+Click` add cursor ·
`Ctrl+G` go to line · `Ctrl+\` jump to matching bracket · `Ctrl+S` save · `Ctrl+K S` save all ·
`Ctrl+K Ctrl+S` save as · `Ctrl+N` new file · `Ctrl+W` close tab · `Ctrl+K Ctrl+W` close all ·
`Ctrl+Shift+T` reopen closed editor · `Ctrl+K Ctrl+K` delete line ·
`Shift+Alt+Down`/`Shift+Alt+Up` copy line down/up · `Alt+Down`/`Alt+Up` move line ·
`Ctrl+Enter`/`Ctrl+Shift+Enter` insert line below/above · `Ctrl+/` toggle comment ·
`Ctrl+K Ctrl+X` trim trailing whitespace · `F8`/`Shift+F8` next/prev diagnostic ·
`Ctrl+Space` completions · `F12` go to definition · `Ctrl+F12` go to implementation ·
`Shift+F12` find references · `Ctrl+Shift+O` document symbols · `F2` rename ·
`Alt+J`/`Alt+K` next/prev git change · `` Ctrl+J ``/`` Ctrl+` `` toggle terminal panel ·
`Ctrl+PageUp`/`Ctrl+PageDown` prev/next terminal · `Ctrl+Q` quit.

## Integrated terminal

A minimizable, tabbed terminal dock lives below the editor. `` Ctrl+J `` (or `` Ctrl+` ``)
opens and focuses it, spawning your shell on first use; press it again to close. Each tab is a
real PTY-backed shell session parsed by a VT100 emulator, so colors, cursor addressing, and
full-screen programs work. While the panel is focused every keystroke — including `Ctrl+C` —
goes to the shell; click the editor to return there, or use the `terminal.*` commands. The
header's `▾`/`▸` control minimizes and restores the dock, `×` closes a tab, and `+` opens a new
one. Mouse-wheel over the panel scrolls its history. It is built to grow (split panes, task
runners, and other bottom-dock contributions can hang off the same panel later).

## Configuration

`~/.config/lumina/config.toml`:

```toml
[settings]
tab_width = 4
sidebar_width = 30
follow_mode = true          # auto-scroll to external edits as an agent writes files
poll_watch = false          # set true on devcontainer/NFS mounts where inotify is unreliable
auto_pairs = true           # auto-close brackets/quotes, type over closers, delete empty pairs
auto_indent = true          # copy indent on newline (brace-aware); dedent on a closing bracket
trim_trailing_whitespace = false  # on save, strip trailing spaces/tabs from every line
insert_final_newline = false      # on save, ensure the file ends with a single newline
git_gutter = true           # per-line add/modify/delete change bar in the gutter (vs HEAD)
icons = false               # Nerd Font file glyphs in the explorer (needs a patched font)
terminal_height = 12        # rows the terminal panel occupies when expanded
# terminal_shell = "bash"   # override the shell (default: $SHELL / /bin/sh, %ComSpec% on Windows)

[keys]
"ctrl+k ctrl+u" = "shout.line"

[lsp]
rust = "rust-analyzer"      # diagnostics; inert unless configured

[theme]                     # override syntax colors by capture name
keyword = "#c678dd"
```

## Plugins

The editor is built on its own plugin system: built-ins register through the same API as
third-party plugins. External plugins live in `~/.config/lumina/plugins/` or
`<project>/.lumina/plugins/`, each a folder with a `plugin.toml` manifest and a guest module.
Two substrates run through the *same* contribution API:

- **Rhai script** (default) — a `main.rhai` returning a list of host actions.
- **WebAssembly** (`runtime = "wasm"`) — a sandboxed `.wasm`/`.wat` guest with **no host
  imports**, fuel-metered against runaway loops, run on the `wasmi` engine.

Both are **deny-by-default**: a plugin declares `capabilities` (`edit`, `ui`, `fs:read`) and
can only take the actions it was granted. See `plugins/` for worked examples — `shout`, `todo`,
`inspector` (Rhai) and `wasm-hello` (WebAssembly).
