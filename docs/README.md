# docs/

- [`ARCHITECTURE.md`](ARCHITECTURE.md) — Lumina's architecture guide: the invariants, crate
  layout, the plugin kernel, and the house Rust style. This is what doc comments mean when
  they cite "CLAUDE.md invariant #N" and "plan §N".
- [`USABILITY.md`](USABILITY.md) — a heuristic evaluation of the interface against Nielsen's
  ten usability heuristics: what the editor tells the user, where it lets them lose work, and
  a prioritised list of fixes framed as commands and plugin contributions.
- [`AUDIT.md`](AUDIT.md) — a historical architecture audit, taken before the plugin migration.
  Read its header first: most of its central findings are resolved.

## README assets

Screenshots referenced by the top-level `README.md`:

| File | What it shows |
|---|---|
| `lumina-editor.webp` | `app.rs` open in lumina: syntax highlighting, tabbed editing, and the clickable file explorer. |
| `lumina-welcome.webp` | The start screen ("a mouse-first terminal code editor") with quick key hints. |
