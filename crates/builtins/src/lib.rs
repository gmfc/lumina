//! `editor_builtins` — the editor's own features, implemented **as plugins** over the
//! contribution API (CLAUDE.md invariant #3, plan §6A). Nothing here is privileged; each
//! feature reaches the editor only through `editor_plugin::Host`.
//!
//! The self-hosting test proves this: disabling a plugin removes exactly its
//! contributions and nothing else.
#![forbid(unsafe_code)]

use editor_plugin::Plugin;

pub mod explorer;

/// The full set of built-in plugins, in registration order. `app` registers these; a user
/// config can filter the list to disable any of them (the litmus test for self-hosting).
pub fn all_builtins() -> Vec<Box<dyn Plugin>> {
    // Plugins are added phase by phase: explorer (Phase 4), find/replace (Phase 6),
    // palette + quick-open (Phase 7), search + sync (Phase 8), lsp (Phase 10).
    vec![Box::new(explorer::ExplorerPlugin::default())]
}
