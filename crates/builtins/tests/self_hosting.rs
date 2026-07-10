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

// The multi-cursor feature is a plugin too (the second feature migrated off the hardcoded
// dispatch). Guarding a *second* plugin keeps the isolation check below operating on a
// multi-plugin set, so "disabling one removes exactly its contributions" is a real assertion.
const MULTICURSOR_ID: &str = "multicursor";
const MULTICURSOR_COMMAND: &str = "cursor.addNextMatch";

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

    // … and nothing unrelated was disturbed (multicursor's `cursor.*` commands survive).
    for id in before.iter().filter(|id| !id.starts_with("explorer.")) {
        assert!(
            reduced.command_ids().any(|c| &c == id),
            "disabling the explorer wrongly removed unrelated command `{id}`"
        );
    }
}

#[test]
fn multicursor_contributes_through_the_public_api() {
    let reg = Registry::with_plugins(all_builtins());
    assert!(
        reg.command_ids().any(|id| id == MULTICURSOR_COMMAND),
        "multi-cursor command missing — is it wired as a plugin (not a hardcoded Command arm)?"
    );
}

#[test]
fn disabling_multicursor_removes_only_its_contributions() {
    let full = Registry::with_plugins(all_builtins());
    let before: Vec<String> = full.command_ids().map(|s| s.to_string()).collect();

    let reduced = Registry::with_plugins(
        all_builtins()
            .into_iter()
            .filter(|p| p.id() != MULTICURSOR_ID),
    );

    // Its `cursor.*` commands are gone …
    assert!(
        !reduced.command_ids().any(|id| id == MULTICURSOR_COMMAND),
        "multi-cursor command still present after disabling — it is hardcoded, not a plugin"
    );
    // … and the explorer's contributions are untouched.
    for id in before.iter().filter(|id| !id.starts_with("cursor.")) {
        assert!(
            reduced.command_ids().any(|c| &c == id),
            "disabling multi-cursor wrongly removed unrelated command `{id}`"
        );
    }
}

/// A migrated plugin owns its keybindings too, not just its commands: the chord travels with
/// the plugin (via `Contributions::keybinding`) so `build_keymap` folds it into the keymap and
/// disabling the plugin unbinds the chord. Guards the keymap-wiring fix for invariant #3.
#[test]
fn migrated_plugins_contribute_their_keybindings() {
    let full = Registry::with_plugins(all_builtins());
    let bound = |reg: &Registry, chord: &str, command: &str| {
        reg.keybindings()
            .iter()
            .any(|kb| kb.chord == chord && kb.command == command)
    };
    assert!(
        bound(&full, "ctrl+d", "cursor.addNextMatch"),
        "multi-cursor's ctrl+d must be contributed through the registry, not the defaults table"
    );
    assert!(
        bound(&full, "alt+j", "git.nextHunk"),
        "git-nav's alt+j must be contributed through the registry, not the defaults table"
    );

    let no_multicursor = Registry::with_plugins(
        all_builtins()
            .into_iter()
            .filter(|p| p.id() != MULTICURSOR_ID),
    );
    assert!(
        !bound(&no_multicursor, "ctrl+d", "cursor.addNextMatch"),
        "disabling multi-cursor must unbind its chord — otherwise the binding is hardcoded"
    );
}
