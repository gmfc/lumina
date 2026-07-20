# LSP PR5 — Document formatting (§6.5)

**Date:** 2026-07-13
**Status:** Approved (first feature PR on the Phase-0 foundation; proceed-to-completion)
**Crates:** `editor-lsp`, `editor-plugin`, `editor-builtins`, `lumina`.

## Why

First user-facing feature built on the completed Phase-0 foundation — a full vertical slice
that proves the architecture: capability gating (PR1) + the Transaction apply pipeline
(rename) + the primitive-twin plugin boundary all compose for a new feature with minimal new
code.

## Design (one command, whole-document formatting)

- **`editor-lsp`**: `Cap::Formatting` + `ServerCaps.formatting`
  (`documentFormattingProvider`); `LspHandle::formatting(uri, tab_size, insert_spaces)` →
  `textDocument/formatting`; `parse_text_edits` (bare `TextEdit[]` result); declare
  `textDocument.formatting: {}` in the client caps.
- **`editor-plugin`**: `LspRequestKind::Formatting` (primitive intent).
- **`editor-builtins`**: `lsp.format` command ("Edit: Format Document", `shift+alt+f`) on the
  existing `LspPlugin` → `Host::lsp_request(Formatting)`. No new plugin, so the self-hosting
  test needs no change.
- **`lumina`**: `Pending::Formatting` (not cancelable — an explicit action — but
  version-guarded like everything else); `request_formatting` gated on `Cap::Formatting`,
  `FormattingOptions` from `config.tab_width` (Lumina indents with spaces);
  `LspEvent::Formatting(Vec<TextEdit>)` → apply to the active doc via the shared
  `apply_workspace_edit` (one atomic Transaction group, invariant #1).

## Non-goals (later)
Range formatting, on-type formatting, format-on-save, `trimTrailingWhitespace`/
`insertFinalNewline`/`trimFinalNewlines` options, cursor preservation through the edits
(the Transaction path already maps selections).

## Testing
- Crate: `parse_text_edits` (skips malformed, handles null); `formatting` capability parse +
  gate.
- App: a `Formatting` response reformats the active buffer (end-to-end through the apply
  pipeline).
- Self-hosting proptest still green (command added to an existing plugin).
- Real clangd: advertises `documentFormattingProvider`, returns edits for ugly C, parsed
  correctly (verified).
