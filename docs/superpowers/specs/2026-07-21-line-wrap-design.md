# Line-Wrap (Soft Word-Wrap) Toggle — Design

**Status:** design
**Date:** 2026-07-21

## Goal

Add a toggle for **soft word-wrap** in the editor pane: when on, long logical lines are broken
across multiple screen rows at word boundaries instead of scrolling horizontally, and the cursor
navigates those wrapped rows visually (Up/Down move one screen row). Off by default; the buffer is
never modified.

## Guiding principle

**Wrap is pure *view* state.** The rope, transactions, char offsets, undo history, and every
`Document` API are untouched. A logical line maps to **≥1 *visual rows*** for display and
navigation. This preserves CLAUDE.md invariants #1 (all mutation via Transaction), #5 (core is
headless), and #6 (coordinate math is pure and unit-testable in `editor-core`).

## Decisions (locked)

| Decision | Choice |
| --- | --- |
| Navigation | **Visual rows** — Up/Down move one screen row; clicks target the visual row |
| Wrap boundary | **Word** — break at the last whitespace before the edge; hard-break a single word longer than the width |
| Scroll granularity | **Visual row (smooth)** — a `scroll_sub` anchor selects which segment of the top logical line is the first visible row |
| Toggle scope | **Global** — one app-wide state flips every open + future doc |
| Default | **Off** (config `line_wrap = false`) |
| Continuation indent | **None** (MVP) — continuation rows start at column 0 |
| Wrap column | **Viewport width only** (MVP) — no configurable wrap column |
| Vim `j`/`k` | **Logical** (MVP) — visual `gj`/`gk` is a follow-up |

## Components

### 1. Core wrap primitive — `crates/core/src/wrap.rs` (new, pure, unit-tested)

```rust
/// Char offsets within `line` (excluding its trailing newline) where each visual row begins.
/// The first element is always 0; the returned length is the number of visual rows (≥1).
/// Breaks at the last whitespace boundary that fits; a single word wider than `width` is
/// hard-broken at the cell that would overflow. `width == 0` yields `vec![0]` (degenerate).
pub fn wrap_segments(line: &str, width: usize, tab_width: usize) -> Vec<usize>;
```

Cell widths come from the existing `editor_core::view::char_cells` model (tabs → next tab stop,
wide/CJK → 2 cells, zero-width clamped to 1) so wrapping agrees with rendering exactly. This is the
single source of truth: rendering, motion, and coordinate mapping all consult it. Not cached in v1
— computed per visible line per frame and on demand for motions (a viewport is a few dozen lines;
revisit only if profiling shows it matters).

### 2. `ViewState` additions — `crates/core/src/view.rs`

```rust
pub wrap: bool,          // per-doc, but the global toggle keeps every doc's copy identical
pub wrap_width: usize,   // laid-out editor text width in cells; the app refreshes it each frame
pub scroll_sub: usize,   // index of the top logical line's first *visible* visual row (0 = whole line)
```

- The app writes `wrap_width` from the last laid-out `regions.editor` width every frame (the same
  pattern as `App::page_height`), so core motions see geometry without a terminal.
- Under wrap, `scroll_col` is pinned to 0 (horizontal scroll is disabled).
- `scroll_sub` is clamped to `< wrap_segments(top_line).len()`; it resets to 0 when `scroll_line`
  changes to a different logical line.

### 3. Visual-row navigation — `crates/core/src/motion`

When `view.wrap` is on, `Motion::Up`/`Down` become segment-aware:

- Compute the caret's `(segment, col_in_segment)` from `wrap_segments(current_line)` +
  `char_to_display_col`.
- `Down`: the next visual row is the caret line's next segment, or segment 0 of the next logical
  line; land at the char whose display column is closest to the preserved `goal_col` within that
  visual row. `Up` is symmetric.
- `Home`/`End` snap to the **visual row's** first/last char (not the whole logical line).
- Left/Right, word motions, and all edits stay char-based and unchanged.

`goal_col` (the existing "sticky column" for vertical motion) is reused verbatim.

### 4. Rendering — `crates/app/src/ui/editor.rs`

The render loop iterates **visual rows** instead of logical lines:

- Starting from `(scroll_line, scroll_sub)`, expand each visible logical line into its
  `wrap_segments`; emit one screen row per segment until the pane is full.
- Gutter: the line number renders on **segment 0**; continuation rows show blank gutter.
- Each segment renders its char slice `[seg_start, seg_end)` with the existing per-cell styling
  (syntax, decorations bucketed per **logical** line, selection, bracket, cursor). Horizontal-scroll
  clipping is skipped under wrap (every segment fits the pane by construction).
- When `view.wrap` is off, the loop is exactly today's 1-row-per-line path (zero behavior change).

### 5. Coordinate mapping — `crates/core/src/view.rs`

`screen_to_char` (clicks) and the caret's `char_to_screen` (hardware cursor placement) walk the
visual layout from `(scroll_line, scroll_sub)` when wrap is on: map a screen row to
`(logical_line, segment)`, then map the column within the segment via `display_col_to_char`. Off,
they use today's direct mapping.

### 6. Toggle — config + command

- **Config**: `line_wrap: bool` in `[settings]` (default `false`), following the existing
  `git_gutter` pattern (`config.rs` default + serialize + parse). It seeds the app-wide default.
- **State**: an app-wide `wrap_enabled: bool` (on `EditorState`, beside `sidebar_visible`). It is
  the source of truth; the app mirrors it into every open doc's `view.wrap` whenever it changes and
  seeds each newly opened doc's `view.wrap` from it. Core reads `doc.view.wrap`.
- **Command**: `view.toggleWrap` (new `Command` variant + `commands.rs` mapping) flips
  `wrap_enabled`, mirrors it to all docs, and (when turning off) clears `scroll_sub`. Bound to
  **Alt+Z** in the default keymap (VS Code parity); also appears in the command palette.

## Data flow

```
key Alt+Z ─▶ view.toggleWrap ─▶ EditorState.wrap_enabled ^= true
                                 └─▶ every doc.view.wrap = wrap_enabled
frame:  app writes doc.view.wrap_width = regions.editor text width
render: ui::editor iterates visual rows via wrap_segments (when doc.view.wrap)
motion: Up/Down consult wrap_segments + goal_col (when doc.view.wrap)
click:  screen_to_char walks the visual layout (when doc.view.wrap)
scroll: ensure_cursor_visible clamps (scroll_line, scroll_sub) in visual-row space
```

## Error handling / edge cases

- `wrap_width == 0` (pane not yet laid out, or zero-width): `wrap_segments` returns `vec![0]` →
  behaves as a single unwrapped row; no panic, no division by zero.
- Empty line: one visual row (segment `[0]`).
- A word longer than `wrap_width`: hard-broken so no row overflows the pane.
- Tabs / wide chars straddling the edge: the segment ends before the char that would overflow (the
  char moves wholly to the next row), keeping every row within `wrap_width`.
- Toggling off restores horizontal scroll (`scroll_col`) and the 1-row-per-line render/motion path.

## Testing

- **`wrap_segments` unit tests** (pure, in `editor-core`): word boundaries; an over-long single
  word hard-breaks; multiple wraps; trailing/leading whitespace; tabs; wide/CJK chars at the
  boundary; empty line; exact-width line (no spurious extra row); `width == 0`.
- **Visual motion tests**: Up/Down step through a wrapped line's segments and across logical-line
  boundaries; `goal_col` is preserved; Home/End hit visual-row edges; with wrap **off**, motions are
  byte-for-byte unchanged.
- **Coordinate round-trip**: `screen_to_char ∘ char_to_screen == identity` across a wrapped
  viewport (extends the existing coordinate suite).
- **Render tests** (`app/tests`): a line longer than the pane renders as N rows with the number on
  row 0 and blank continuation gutters; toggling on/off changes the row count; a `.txt` line with a
  decoration still paints across the wrap.
- **Toggle tests**: `view.toggleWrap` flips `wrap_enabled` and every doc's `view.wrap`; a
  newly-opened doc inherits the current state; config `line_wrap = true` seeds it on.

## Out of scope (follow-ups)

- `wrappingIndent` (aligning continuation rows under the code).
- Configurable wrap column (e.g. wrap at 80 regardless of pane width).
- Visual `gj`/`gk` in Vim mode.
- Wrap-aware `PageUp`/`PageDown` by visual rows (v1 keeps them logical-line based).
