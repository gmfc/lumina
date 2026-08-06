# Large Files, Binary Files, and Plugin File Viewers — Design

**Status:** design
**Date:** 2026-08-05

## Problem

`lmn some.pdf` (or clicking one in the explorer) does the worst possible thing today:

`App::open_path` → `files::load` → `fs::read` (whole file into memory) →
`String::from_utf8_lossy` (a second whole-file copy, every non-UTF-8 byte becoming U+FFFD) →
`Document::from_str` (a `replace("\r\n", "\n")` copy plus a rope build) → `fingerprint` (an
O(n) hash). Four full passes and ~3× the file size resident, on the UI thread, before a single
frame is drawn. The user then gets a tab full of replacement characters that they can *edit*
and *save* — corrupting the file.

The same path is taken by session restore (`App::new`), so one accidental PDF poisons every
subsequent launch of that project.

Two things are missing, and they are the two the report asks for:

1. **A policy**: some files should not become text buffers at all, and the editor should say so
   clearly instead of hanging or lying.
2. **A seam**: some of those files *can* be shown usefully, by code that knows the format. That
   belongs in a plugin, not in `lumina`.

## Guiding principles

- **Probe before you read.** The decision to load a file must cost O(header), never O(file).
  Nothing reads a whole file until policy says it is text and small enough.
- **Refusal is a view, not an error.** A rejected file opens a *tab* that explains itself and
  offers a way forward, rather than a status-bar flash that scrolls away.
- **Viewers are plugins.** `lumina` gains no knowledge of PDF. It gains a contribution kind;
  the PDF code lives in `editor-builtins` beside the explorer and reaches the editor only
  through `Host` (invariant #3).
- **Non-text tabs are inert.** A tab that does not hold a text buffer can never be written to
  disk, reloaded into, or edited. Guarded at every mutation site, not by convention.

## Decisions (locked)

| Decision | Choice |
| --- | --- |
| Detection | **Head probe** — `metadata().len()` + first 8 KiB; magic numbers, then a NUL scan |
| UTF-16 | BOM-detected UTF-16 is **text**, and skips the NUL scan (its NULs are structural) |
| Size limit | `max_file_size_mb`, default **64**; refusal is **overridable** ("Open Anyway") |
| Degraded mode | `large_file_mb`, default **8**; suppresses syntax, git gutter, and LSP |
| Non-text tab | A placeholder `Document` + a `TabView` side table — the **Settings tab pattern** |
| Viewer output | `Vec<PanelLine>` — the **existing** plugin→render channel, drawn full-pane |
| Viewer claim | By **extension**, first registered wins; an empty extension list = explicit-only |
| Built-in viewers | `pdf` (text extraction) and `hexview` (any file, bounded) |

## 1. Probe and policy — `crates/app/src/files.rs`

```rust
/// Bytes sniffed from a file's head to classify it (what git uses).
const SNIFF_BYTES: usize = 8 * 1024;

/// What a cheap head-probe learned about a file, without reading it whole.
pub struct Probe { pub len: u64, pub kind: FileKind }

pub enum FileKind {
    Text,
    /// Binary content. `label` names the format when a magic number matched
    /// ("PDF document"), else the generic "binary data".
    Binary { label: &'static str },
}

/// Why lumina declined to load a file into a text buffer.
pub enum Refusal {
    Binary { label: &'static str, len: u64 },
    TooLarge { len: u64, limit: u64 },
}

/// Size policy, from config. `max_bytes == 0` disables the ceiling.
pub struct Limits { pub max_bytes: u64, pub large_bytes: u64 }

/// Read `path`'s header and classify it. O(header), never O(file).
pub fn probe(path: &Path) -> Result<Probe>;

/// The single open chokepoint: probe, apply `limits`, and load only if it passes.
pub fn open(path: &Path, limits: &Limits) -> Result<Opened>;

pub enum Opened {
    /// Safe to edit. `doc.large` is already set from `limits.large_bytes`.
    Text(Box<Document>),
    Refused(Refusal),
}
```

Classification order inside `probe`:

1. **BOM first.** `FF FE` / `FE FF` → UTF-16, which is *text* and is exempt from the NUL scan.
   `EF BB BF` → UTF-8 with BOM; scan the remainder.
2. **Magic numbers.** A table of `(prefix, label)`: `%PDF-` → "PDF document", `\x89PNG` →
   "PNG image", `\x7FELF` → "ELF executable", `PK\x03\x04` → "ZIP archive", `SQLite format 3` →
   "SQLite database", … Named formats produce a *useful* refusal message.
3. **NUL scan** over the sniff window → the generic `"binary data"`.
4. Otherwise **text**. Invalid UTF-8 alone does not condemn a file (Latin-1 source still opens,
   lossily, exactly as today).

`Opened::Text` sets `doc.large = len >= limits.large_bytes` so downstream features can degrade
without re-statting.

Every caller that turns a path into a tab goes through `files::open`: `App::open_path`
(explorer, quick-open, project search, LSP navigation, `Host::open_path`) and `App::new`
(the CLI argument **and** session restore).

## 2. Degraded mode for large text files — `Document::large`

`editor-core` gains one field:

```rust
/// Set at load when the file exceeded the "large file" threshold: expensive per-document
/// features stay off for it. Pure metadata — the rope and every text API are unaffected.
pub large: bool,
```

Three suppressions, each at the site that already owns the cost:

| Feature | Site | Behavior when `large` |
| --- | --- | --- |
| tree-sitter highlighting | `EditorState::update_highlights` | no highlighter is created |
| git change gutter | `App::request_git_status` | no diff job is spawned |
| language server | `App::sync_lsp_document` | no `didOpen`/`didChange` is sent |

The status bar reports the mode once, so the absence of colour is explained rather than
mysterious.

## 3. Non-text tabs — `TabView`

The Settings tab already establishes the pattern: a placeholder `Document` in the normal tab
machinery, plus app-side state and a render branch. This generalizes it rather than adding a
second mechanism.

```rust
// crates/app/src/editor.rs
/// Tabs that render something other than their (empty placeholder) buffer, keyed by the
/// `DocId` backing them. Absent from the map ⇒ an ordinary text tab.
pub(crate) tab_views: HashMap<DocId, TabView>,

pub(crate) enum TabView {
    /// A file lumina declined to load into a text buffer.
    Notice { path: PathBuf, refusal: Refusal },
    /// A plugin-owned viewer for `path`.
    Viewer(ViewerTab),
}

pub(crate) struct ViewerTab {
    pub viewer_id: String,
    pub path: PathBuf,
    /// Published by the owning plugin via `Host::set_viewer_content`.
    pub content: ViewerContent,
    pub scroll: usize,
}
```

The placeholder document **keeps its path**, so `find_by_path` de-duplicates tabs, the tab bar
names it, and the session records it — all for free.

That path is also the hazard: an empty buffer pointed at a PDF must never be written. Every
mutation site is guarded by `EditorState::is_tab_view(id)`:

| Site | Guard |
| --- | --- |
| `App::save_active` | refuses with "… is not a text buffer" |
| `App::save_all` | skips view tabs |
| `App::on_file_changed` | re-renders the viewer / re-probes the notice instead of reloading text |
| `App::apply_workspace_edit` | skips view tabs (an LSP rename cannot edit one) |
| key routing (`App::tab_view_key`) | scroll keys consumed, text entry swallowed |
| `App::request_git_status`, LSP sync | never run for a view tab (no language, no text) |

`Command::OpenAnyway` (`file.openAnyway`) replaces a **notice** tab with a real text tab,
bypassing the size ceiling. Binary refusals are not overridable this way — a NUL-bearing buffer
cannot round-trip through the UTF-8 rope, so "open anyway" would silently corrupt on save. The
notice points at the hex viewer instead.

## 4. Viewer contributions — `editor-plugin`

A new contribution kind, wired exactly like `PanelSpec`:

```rust
// contribution.rs
pub struct ViewerSpec {
    pub id: String,
    pub title: String,
    /// Lowercase extensions claimed, without the dot. Empty ⇒ explicit-open only.
    pub extensions: Vec<String>,
}
pub struct Contributions { …, pub viewers: Vec<ViewerSpec> }

// viewer.rs
/// What a viewer publishes for its tab: styled rows the app draws full-pane.
pub struct ViewerContent { pub status: Option<String>, pub lines: Vec<PanelLine> }

// registry.rs
impl Registry {
    pub fn viewer_for_extension(&self, ext: &str) -> Option<&ViewerSpec>;
    pub fn render_viewer(&mut self, viewer_id: &str, doc: DocId, path: &Path,
                         host: &mut dyn Host) -> bool;
}

// Plugin trait
fn render_viewer(&mut self, _viewer_id: &str, _doc: DocId, _path: &Path,
                 _host: &mut dyn Host) {}

// Host
fn set_viewer_content(&mut self, doc: DocId, content: ViewerContent);
fn open_viewer(&mut self, path: &Path, viewer_id: &str);
/// The active tab's file path — including notice/viewer tabs, which have no text buffer.
fn active_path(&self) -> Option<PathBuf>;
```

Reusing `PanelLine`/`Span` means a viewer's output flows through the *same* styled-row channel
the explorer and search results already use, and the theme maps its style keys the same way.

The external tier gets the same kind: `[[viewers]]` in `plugin.toml`, dispatched to a
`render_viewer(id, ctx)` Rhai function whose returned lines are published under the existing
`ui` capability, with the file's bytes handed over (capped at 1 MB) only under `fs:read`. No
privileged back door — a built-in viewer and a script viewer take the same road. `plugins/csvview`
is the worked example.

While wiring this up, the Rhai sandbox turned out to leave expression depth at Rhai's default,
which is *halved in debug builds* — so a plugin that compiles against a released `lmn` fails to
load under `cargo run`, with an error no plugin author could diagnose. The limits are now pinned
explicitly (`set_max_expr_depths(64, 32)`) alongside the operation/string/array caps.

## 5. Rendering — `crates/app/src/ui/tabview.rs`

`ui::draw` gains one branch beside the existing Settings branch:

```
settings_active()      → render_settings
active_tab_view()      → render_tab_view      ← new
otherwise              → render_editor
```

- **Notice**: a centered box — the format label, file name, size, the reason, and hint rows
  built from `(command id, label)` pairs whose chords are looked up live in the keymap (the
  welcome screen's pattern), so only commands that actually exist are offered.
- **Viewer**: a title row, then the published lines, scrolled by `ViewerTab::scroll`, with a
  scroll indicator. Up/Down/PageUp/PageDown/Home/End and the wheel scroll it.

Both are pure functions of state (invariant #8): the plugin publishes content on open, the
renderer only reads it.

## 6. Built-in viewers — `editor-builtins`

### `hexview` (no new dependencies)

Claims no extensions; opened explicitly via `view.openAsHex`, which the notice tab advertises.
Renders a classic `offset  hex bytes  |ascii|` dump of the first `N` bytes (bounded, so a 4 GB
file is as cheap as a 4 KB one), with the truncation stated in the status row. This makes
*every* binary refusal actionable, including formats no viewer understands.

### `pdf` (one new dependency: `flate2`, `rust_backend`)

Claims `.pdf` and extracts document text.

- **Object scan, not xref.** Scanning for `N G obj … endobj` is resilient to the broken,
  linearized, and incrementally-updated files an xref walk chokes on.
- **Object streams** (`/Type /ObjStm`, PDF 1.5+) are expanded, since modern producers put page
  dictionaries there — skipping them would fail on most real PDFs.
- **Page order** from the catalog's `/Pages` → `/Kids` walk, falling back to object number.
- **Text operators** `Tj`, `TJ`, `'`, `"` inside `BT`/`ET`, with `Td`/`TD`/`T*`/`TL`/`Tm`
  tracked to recover line breaks; `/ToUnicode` CMaps (`bfchar`/`bfrange`) decode embedded-font
  byte strings, falling back to WinAnsi.
- **Honest failure.** `/Encrypt` → "encrypted; text extraction is not supported". No extractable
  text → a structural summary (page count, `/Info` metadata) plus a pointer at the hex viewer.
- **Panic-free by construction.** Every offset is bounds-checked and every loop bounded; a
  malformed PDF yields a short document, never a crash. The workspace already forbids `unsafe`.

Both register in `all_builtins()` and are therefore user-disableable in `[plugins]` like every
other built-in.

## 7. Configuration

| Key | Default | Range | Meaning |
| --- | --- | --- | --- |
| `max_file_size_mb` | 64 | 1–4096 | Above this, refuse with an overridable notice |
| `large_file_mb` | 8 | 1–4096 | Above this, open but suppress syntax / git / LSP |

Both follow the `git_gutter` pattern end to end: `Config` field + default, `apply_settings`
parse with clamping, `write_to` serialization, and a row in the Settings tab.

## Error handling / edge cases

- **Unreadable/absent file** — `probe` returns the IO error; the existing "Open failed: …"
  status path is unchanged.
- **Empty file** — text (no magic, no NULs).
- **File shorter than a magic prefix** — prefix comparison is on the available slice; no panic.
- **UTF-16 with BOM** — text; the NUL scan is skipped, so `.utf16` files still open.
- **File grows past the limit while open** — already-open buffers are unaffected; the limit is
  an open-time policy only.
- **A file forced open past the ceiling** is always in degraded mode, even when it sits under
  `large_file_mb`. The user has already been told it is over the limit; re-enabling a
  tree-sitter parse and a whole-buffer `didOpen` on that file is the opposite of what "open
  anyway" asks for. (This also settles the `max_file_size_mb` < `large_file_mb` case, where the
  ceiling would otherwise admit a file the "large" threshold does not cover.)
- **A viewer plugin is disabled** — its extension claim disappears with it, so `.pdf` falls back
  to the binary notice.
- **A viewer claims a *text* extension** (the `csvview` example claims `.csv`) — `file.openAsText`
  is the escape hatch, advertised in the viewer tab's header. Without it a viewer could take a
  file type hostage for as long as it is installed.
- **A file changes on disk while open** — the reload re-applies the same policy, so a log that
  grows past the ceiling (or turns binary) is *not* reloaded, and one that crosses the degraded
  threshold picks it up. The watcher was otherwise a back door into the unbounded read.
- **`metadata().len()` lies** (a `/proc` entry, a file growing between stat and read) — the read
  is capped at the ceiling and overshoot is a refusal, because a truncated buffer would destroy
  everything past the cut on the first save.
- **Two plugins claim `.pdf`** — first registered wins, deterministically (registration order).
- **A viewer publishes nothing** — the tab renders its status row and an empty body, not a blank
  screen.
- **Save on a view tab** — refused with a message; the file on disk is never touched.

## Testing

- **`probe`** — each magic number is labelled; NUL detection; a UTF-16 BOM file is *text*; an
  empty file is text; a file shorter than the longest magic prefix; a plain source file is text.
- **`files::open`** — a text file under both limits loads with `large == false`; between the
  thresholds loads with `large == true`; over `max_bytes` returns `TooLarge`; a PDF returns
  `Binary { label: "PDF document" }` **without** reading the body.
- **Open routing** — opening a binary creates a *notice* tab (no text buffer, `is_tab_view`);
  `file.openAnyway` on an oversize text file replaces it with a real buffer; a binary notice
  offers no override.
- **Inertness** — `save_active` on a view tab leaves the bytes on disk byte-for-byte unchanged
  (the regression that matters most); `save_all` skips it; an external change re-renders rather
  than reloading text.
- **Degraded mode** — a doc over `large_file_mb` gets no highlighter, no git job, no `didOpen`.
- **Registry** — a viewer contribution is claimable by extension, is dropped with its plugin,
  and loses the claim when disabled (extends the self-hosting guard).
- **Render** — a notice tab draws the label, size, and hints; a viewer tab draws its published
  lines and scrolls; both leave the text renderer untouched when no view tab is active.
- **`hexview`** — offsets, byte columns, and the ASCII gutter for a known byte string; the
  truncation notice past the cap.
- **`pdf`** — a hand-written uncompressed PDF fixture extracts its text; the same content
  Flate-compressed extracts identically; an object-stream fixture is expanded; `/Encrypt`
  reports encryption; a truncated/garbage file returns a summary instead of panicking.

## Out of scope (follow-ups)

- Streaming/memory-mapped buffers for genuinely huge text files (the ceiling is a refusal, not
  a windowed reader).
- Rendering PDF *pages* (glyph rasterization) rather than extracted text.
- Image viewers via terminal graphics protocols (Kitty/Sixel/iTerm2).
- Editing through a viewer (viewers are read-only by construction).
- Per-language size thresholds, and a "large file" hint in the explorer before opening.
