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

pub mod explorer;

/// The full set of built-in plugins, in registration order. `app` registers these; a user
/// config can filter the list to disable any of them (the litmus test for self-hosting).
pub fn all_builtins() -> Vec<Box<dyn Plugin>> {
    all_builtins_with(false)
}

/// Like [`all_builtins`], but with runtime options threaded in from user config (e.g. whether
/// the explorer draws Nerd Font glyphs).
pub fn all_builtins_with(icons: bool) -> Vec<Box<dyn Plugin>> {
    // Only the explorer is a plugin today. find/replace, palette + quick-open, search + sync, and
    // lsp were planned as plugins (plan §6A) but still live in `editor-app` as hardcoded commands
    // — each needs new `Host` ports first. See the migration roadmap in `docs/AUDIT.md`.
    vec![Box::new(explorer::ExplorerPlugin::new(icons))]
}
