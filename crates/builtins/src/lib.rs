//! `editor_builtins` — the editor's own features, implemented **as plugins** over the
//! contribution API (CLAUDE.md invariant #3, plan §6A). Nothing here is privileged; each
//! feature reaches the editor only through `editor_plugin::Host`.
//!
//! **Current state (see `docs/AUDIT.md`).** Only the **explorer** is self-hosted as a plugin
//! today. The editor's other features — find/replace, palette, quick-open, project search, LSP,
//! git, terminal, vim — still live in `editor-app` as hardcoded commands, because the `Host` port
//! does not yet expose what they need (overlays, pickers, decorations, background jobs, LSP). The
//! modularization roadmap in `docs/AUDIT.md` widens `Host` and migrates them here one at a time.
//!
//! The self-hosting test proves the invariant *for the explorer*: disabling that plugin removes
//! exactly its contributions and nothing else. Generalize it as each feature moves over.

use editor_plugin::Plugin;

pub mod diagnostics;
pub mod explorer;
pub mod find;
pub mod git_nav;
pub mod lsp;
pub mod multicursor;
pub mod palette;
pub mod project_search;
pub mod terminal;
pub mod theme;

/// The full set of built-in plugins, in registration order. `app` registers these; a user
/// config can filter the list to disable any of them (the litmus test for self-hosting).
pub fn all_builtins() -> Vec<Box<dyn Plugin>> {
    all_builtins_with(false)
}

/// Like [`all_builtins`], but with runtime options threaded in from user config (e.g. whether
/// the explorer draws Nerd Font glyphs).
pub fn all_builtins_with(icons: bool) -> Vec<Box<dyn Plugin>> {
    // Feature-by-feature migration onto the plugin system (docs/AUDIT.md roadmap). Extracted so
    // far: the explorer, multi-cursor, git-change navigation, find/replace, the command palette +
    // quick-open + goto-line, project search, the theme toggle, the LSP request commands,
    // diagnostics, and the terminal-dock commands (all reach the editor only through `Host`).
    // Still hardcoded in `editor-app`: the completion widget + LSP response handling (transport +
    // goto/hover/refs/rename/symbols stay app-side), the terminal PTY/vt100/render machinery, and
    // vim.
    vec![
        Box::new(explorer::ExplorerPlugin::new(icons)),
        Box::new(multicursor::MultiCursorPlugin),
        Box::new(git_nav::GitNavPlugin),
        Box::new(find::FindReplacePlugin::default()),
        Box::new(palette::PalettePlugin::default()),
        Box::new(project_search::ProjectSearchPlugin::default()),
        Box::new(theme::ThemePlugin),
        Box::new(lsp::LspPlugin::default()),
        Box::new(diagnostics::DiagnosticsPlugin::default()),
        Box::new(terminal::TerminalPlugin),
    ]
}
