#![cfg(feature = "proptests")]
//! INVARIANT TEST (not property-based): the editor is genuinely self-hosted on its plugin API.
//! The explorer is a plugin, not a hardcoded feature. Proof: with all built-ins enabled the
//! explorer's command + panel are present in the registry; disabling the explorer plugin
//! removes exactly those and nothing else. This is the guardrail behind invariant #3.
//! If either half fails, something is wired into `app` directly instead of contributed.
//!
//! Repo path:   crates/builtins/tests/self_hosting.rs
//! Activation:  Phase 7 (contribution registry + built-ins as plugins). Enable via CI's `proptests` job.
//! Requires:    `proptests = []` in editor-builtins's [features].
//!
//! Placeholder API — align with your final crates. Assumed:
//!   editor_builtins::all_builtins() -> Vec<Box<dyn Plugin>>   // the built-in plugin set
//!   editor_plugin::Registry::with_plugins(iter) -> Registry
//!   Registry::command_ids() / Registry::panel_ids() -> impl Iterator<Item = String/&str>
//!   Plugin::id() -> &str

use editor_builtins::all_builtins;
use editor_plugin::Registry;

// Adjust these to your actual explorer plugin's declared ids.
const EXPLORER_ID: &str = "explorer";
const EXPLORER_PANEL: &str = "explorer.tree";
const EXPLORER_COMMAND: &str = "explorer.revealActiveFile";

#[test]
fn builtin_contributes_through_the_public_api() {
    let reg = Registry::with_plugins(all_builtins());
    assert!(
        reg.command_ids().any(|id| id == EXPLORER_COMMAND),
        "explorer command missing — is the explorer actually wired as a plugin?"
    );
    assert!(
        reg.panel_ids().any(|id| id == EXPLORER_PANEL),
        "explorer panel missing — is the explorer actually wired as a plugin?"
    );
}

#[test]
fn disabling_the_explorer_removes_only_its_contributions() {
    let full = Registry::with_plugins(all_builtins());
    let before: Vec<String> = full.command_ids().map(|s| s.to_string()).collect();

    // Disable the explorer exactly the way a user config would: drop it from the plugin set.
    let reduced =
        Registry::with_plugins(all_builtins().into_iter().filter(|p| p.id() != EXPLORER_ID));

    // Its contributions are gone …
    assert!(
        !reduced.command_ids().any(|id| id == EXPLORER_COMMAND),
        "explorer command still present after disabling the plugin — it is hardcoded, not a plugin"
    );
    assert!(
        !reduced.panel_ids().any(|id| id == EXPLORER_PANEL),
        "explorer panel still present after disabling the plugin — it is hardcoded, not a plugin"
    );

    // … and nothing unrelated was disturbed.
    for id in before.iter().filter(|id| !id.starts_with("explorer.")) {
        assert!(
            reduced.command_ids().any(|c| &c == id),
            "disabling the explorer wrongly removed unrelated command `{id}`"
        );
    }
}
