# Right-Click Context Menu (plugin-extensible) — Design

**Status:** approved, implementing.
**Date:** 2026-07-15.
**Goal:** right-clicking in the editor opens a context menu of relevant actions (LSP navigation /
refactor / info + clipboard), anchored at the cursor. The menu is **plugin-extensible** — any
command becomes a menu entry through the contribution API — and **context-gated** (items whose
`when` predicate fails are hidden).

## Decisions (locked)

| # | Decision | Choice |
|---|----------|--------|
| D1 | Extensibility | New `menu_item` contribution on `Contributions::builder`, declared like commands/keybindings. An item names a **command id** and routes through the existing `exec_id` dispatch. |
| D2 | Default items | Navigation + Refactor + Info + Edit groups, contributed by their owning plugins (`lsp`, `code_action`, `clipboard`). |
| D3 | Inapplicable items | **Hidden** — filtered by a `when` predicate evaluated at open time. |
| D4 | `when` model | A small type-safe enum (`Always`/`HasSelection`/`LspEnabled`/`LspOnWord`), evaluated app-side against real state (`is_ready(lang)`, selection, cursor-on-word). |
| D5 | Right-click caret | Place the caret at the click; keep an existing selection if the click lands inside it. |

## Architecture

- **`editor-plugin`** — the declarative contribution type (`MenuItemSpec`, `MenuGroup`, `MenuWhen`),
  aggregated by the `Registry` (invariant #3: features contribute through the same API).
- **`editor-app`** — owns the menu at right-click: builds it from `registry.menu_items()`, filters
  by `when`, sorts by group, opens an `Overlay::ContextMenu`, and renders/navigates/dismisses it.
  Activation is `exec_id(command)` — the one dispatch path (invariant #4). Render is a pure function
  of the overlay state (invariant #8).
- **built-in plugins** — `lsp`, `code_action`, `clipboard` each declare their menu items. The
  defaults *are* the extensibility mechanism exercised.

No `editor-core` change. No new deps.

## Contribution type (`crates/plugin/src/contribution.rs`)

```rust
/// Where a menu item sits (ordering + a separator between groups). `Ord` drives the group sort.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MenuGroup { Navigation, Refactor, Info, Edit }

/// The context predicate deciding whether an item is shown (evaluated app-side at open time).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuWhen {
    Always,        // always shown (e.g. Paste)
    HasSelection,  // a non-empty selection exists (Cut/Copy)
    LspEnabled,    // a server is Running for the active doc's language (Code Action/Format/Symbols)
    LspOnWord,     // LspEnabled AND the caret is on a symbol (Definition/References/Rename/Hover)
}

pub struct MenuItemSpec { pub command: String, pub label: String, pub group: MenuGroup, pub when: MenuWhen }
```

- `Contributions` gains `menu_items: Vec<MenuItemSpec>`; `ContributionsBuilder` gains
  `menu_item(command, label, group, when)`.
- `Registry` aggregates them (`add` extends `menu_items`) + exposes `menu_items() -> &[MenuItemSpec]`.
  No owner map — items route by command id through the existing `command_owner`/`exec_id`.
- Re-export `MenuItemSpec`/`MenuGroup`/`MenuWhen` from `crates/plugin/src/lib.rs`.

## App: open / evaluate / render / dismiss

- **State** (`crates/app/src/editor.rs`): a new `Overlay::ContextMenu { x: u16, y: u16, items:
  Vec<ContextMenuItem>, selected: usize }` variant (reuses all `overlay` key/render/priority
  plumbing), where `ContextMenuItem { label: String, command: String, first_in_group: bool }`.
- **Open** (`crates/app/src/app/mouse.rs`): add `MouseEventKind::Down(MouseButton::Right) =>
  self.mouse_right_down(col, row)`. `mouse_right_down` (guarded by `in_rect(regions.editor)`):
  place the caret at `editor_offset_at(col,row)` (keep the selection if the offset is inside it),
  then `self.open_context_menu(col, row)`.
- **Build + `when`** (`crates/app/src/app/context_menu.rs`, new): `open_context_menu` reads
  `self.registry.menu_items()`, keeps those whose `when` holds, sorts by `MenuGroup` order, marks
  the first item of each group, and sets the `Overlay::ContextMenu`. `when` helpers on `App`:
  `menu_when_holds(when)` dispatching to `active_server_ready` (active doc's language →
  `self.lsp.is_ready(lang)`), `active_has_selection`, and `cursor_on_word` (the
  `document_highlight.rs::cursor_on_word` recipe: `is_word` at/adjacent to the primary head).
- **Keys** (`crates/app/src/app/overlay.rs` `overlay_key`): a `ContextMenu` arm — Up/Down move
  `selected` (wrap), Enter → clear the overlay then `exec_id(items[selected].command)`, Esc → clear.
- **Render** (`crates/app/src/ui/overlays.rs`): `render_context_menu` — a positioned box at
  `(x,y)`, clamp/flip to fit the body (mirror `render_completion` at `ui/pickers.rs:64-70`), `Clear`,
  draw each label with a thin separator above `first_in_group` rows and the `selected` row
  highlighted. Returns the per-item `Vec<Rect>` for hit-testing.
- **Regions + click** (`crates/app/src/ui.rs`): `Regions` gains `context_menu: Option<Vec<Rect>>`,
  set from `render_context_menu`'s return in `draw`. In `mouse_left_down`, **before** the editor
  branch: if a context menu is open, a click on item rect *i* runs `items[i].command` + closes; a
  click outside closes it (the first mouse-interactive overlay, following the `lsp_status` idiom).

## Default menu items (contributed by plugins)

| Plugin | `.menu_item(...)` | group / when |
|---|---|---|
| `lsp` (`builtins/src/lsp.rs`) | Go to Definition / Implementation / Type Definition / Find All References | Navigation / `LspOnWord` |
| `lsp` | Rename Symbol; Format Document | Refactor / `LspOnWord`, `LspEnabled` |
| `lsp` | Show Hover; Symbols in File | Info / `LspOnWord`, `LspEnabled` |
| `code_action` (`builtins/src/code_action.rs`) | Code Action / Quick Fix | Refactor / `LspEnabled` |
| `clipboard` (`builtins/src/clipboard.rs`) | Cut; Copy; Paste | Edit / `HasSelection`, `HasSelection`, `Always` |

(Command ids reuse the existing ones: `lsp.gotoDefinition`, `lsp.references`, `lsp.rename`,
`lsp.format`, `lsp.hover`, `lsp.documentSymbols`, `lsp.codeAction`, and the clipboard plugin's
cut/copy/paste ids — verified against `contributions()` before wiring.)

## Tests

- Contribution: a plugin declaring `menu_item`s surfaces them through `registry.menu_items()`.
- `when` evaluation: `LspOnWord`/`LspEnabled` hidden with no running server; `HasSelection` gated on
  a real selection; `Always` always present.
- Right-click: opens the menu at the click, places the caret, keeps a selection clicked inside.
- Navigation: Up/Down/Enter runs the selected command; Esc + click-outside dismiss; item-click runs it.
- Render: the menu draws its items + group separators, positioned (not centered).

## Non-goals

Submenus (Code Action stays a single item that opens the existing action list); general `when`
expression language (a fixed enum only); serializing menu items from external `plugin.toml`
(script plugins map a keyword later); toolbar/menubar surfaces.
