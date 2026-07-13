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

// Find/replace is a plugin too (over the prompt + decorations ports).
const FIND_ID: &str = "find";
const FIND_COMMAND: &str = "search.find";

// The command palette + quick-open (over the picker + command-enumeration ports).
const PALETTE_ID: &str = "palette";
const PALETTE_COMMAND: &str = "view.commandPalette";

// Project search (over the background-job + prompt + panel ports).
const SEARCH_ID: &str = "project-search";
const SEARCH_COMMAND: &str = "search.project";
const SEARCH_PANEL: &str = "search.results";

// The LSP request commands (over the lsp_request effect-queue + prompt ports).
const LSP_ID: &str = "lsp";
const LSP_COMMAND: &str = "lsp.gotoDefinition";

// The terminal-dock commands (over the terminal_op effect-queue).
const TERMINAL_ID: &str = "terminal";
const TERMINAL_COMMAND: &str = "terminal.toggle";

// Diagnostics (owns the model; over the LspDiagnostics event + decorations + status ports).
const DIAGNOSTICS_ID: &str = "diagnostics";
const DIAGNOSTICS_COMMAND: &str = "lsp.nextDiagnostic";

// The completion widget (owns lsp.completion; over the POPUP + LspCompletion ports).
const COMPLETION_ID: &str = "completion";
const COMPLETION_COMMAND: &str = "lsp.completion";

// Clipboard copy/cut/paste (over the clipboard_read/clipboard_write Host port).
const CLIPBOARD_ID: &str = "clipboard";
const CLIPBOARD_COMMAND: &str = "edit.paste";

// The vim modal layer (owns vim.enable/disable/toggle; intercepts keys via capture_key).
const VIM_ID: &str = "vim";
const VIM_COMMAND: &str = "vim.toggle";

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

#[test]
fn find_contributes_through_the_public_api() {
    let reg = Registry::with_plugins(all_builtins());
    assert!(
        reg.command_ids().any(|id| id == FIND_COMMAND),
        "find command missing — is find/replace wired as a plugin (not app Command arms)?"
    );
}

#[test]
fn disabling_find_removes_only_its_contributions() {
    let full = Registry::with_plugins(all_builtins());
    let before: Vec<String> = full.command_ids().map(|s| s.to_string()).collect();

    let reduced = Registry::with_plugins(all_builtins().into_iter().filter(|p| p.id() != FIND_ID));

    // Its `search.find`/`search.replace`/… commands are gone …
    assert!(
        !reduced.command_ids().any(|id| id == FIND_COMMAND),
        "find command still present after disabling — it is hardcoded, not a plugin"
    );
    // … and nothing unrelated was disturbed (the app's `search.project` isn't a find contribution).
    for id in before.iter().filter(|id| !id.starts_with("search.")) {
        assert!(
            reduced.command_ids().any(|c| &c == id),
            "disabling find wrongly removed unrelated command `{id}`"
        );
    }
}

#[test]
fn palette_contributes_through_the_public_api() {
    let reg = Registry::with_plugins(all_builtins());
    assert!(
        reg.command_ids().any(|id| id == PALETTE_COMMAND),
        "command-palette missing — is the palette wired as a plugin (not an app Command arm)?"
    );
}

#[test]
fn disabling_palette_removes_only_its_contributions() {
    let full = Registry::with_plugins(all_builtins());
    let before: Vec<String> = full.command_ids().map(|s| s.to_string()).collect();

    let reduced =
        Registry::with_plugins(all_builtins().into_iter().filter(|p| p.id() != PALETTE_ID));

    assert!(
        !reduced.command_ids().any(|id| id == PALETTE_COMMAND),
        "palette command still present after disabling — it is hardcoded, not a plugin"
    );
    // Nothing unrelated removed (the app's `view.gotoLine` isn't a palette contribution).
    for id in before.iter().filter(|id| !id.starts_with("view.")) {
        assert!(
            reduced.command_ids().any(|c| &c == id),
            "disabling palette wrongly removed unrelated command `{id}`"
        );
    }
}

#[test]
fn project_search_contributes_through_the_public_api() {
    let reg = Registry::with_plugins(all_builtins());
    assert!(
        reg.command_ids().any(|id| id == SEARCH_COMMAND),
        "project-search command missing — is it wired as a plugin?"
    );
    assert!(
        reg.panel_ids().any(|id| id == SEARCH_PANEL),
        "project-search results panel missing — is it wired as a plugin?"
    );
}

#[test]
fn disabling_project_search_removes_only_its_contributions() {
    let full = Registry::with_plugins(all_builtins());
    let before: Vec<String> = full.command_ids().map(|s| s.to_string()).collect();

    let reduced =
        Registry::with_plugins(all_builtins().into_iter().filter(|p| p.id() != SEARCH_ID));

    assert!(
        !reduced.command_ids().any(|id| id == SEARCH_COMMAND),
        "project-search command still present after disabling — it is hardcoded, not a plugin"
    );
    assert!(
        !reduced.panel_ids().any(|id| id == SEARCH_PANEL),
        "project-search panel still present after disabling — it is hardcoded, not a plugin"
    );
    // Nothing unrelated removed (find's `search.find` isn't a project-search contribution).
    for id in before.iter().filter(|id| id.as_str() != SEARCH_COMMAND) {
        assert!(
            reduced.command_ids().any(|c| &c == id),
            "disabling project-search wrongly removed unrelated command `{id}`"
        );
    }
}

#[test]
fn lsp_contributes_through_the_public_api() {
    let reg = Registry::with_plugins(all_builtins());
    assert!(
        reg.command_ids().any(|id| id == LSP_COMMAND),
        "lsp command missing — is the LSP request feature wired as a plugin?"
    );
}

#[test]
fn disabling_lsp_removes_only_its_contributions() {
    let full = Registry::with_plugins(all_builtins());
    let before: Vec<String> = full.command_ids().map(|s| s.to_string()).collect();

    let reduced = Registry::with_plugins(all_builtins().into_iter().filter(|p| p.id() != LSP_ID));

    assert!(
        !reduced.command_ids().any(|id| id == LSP_COMMAND),
        "lsp command still present after disabling — it is hardcoded, not a plugin"
    );
    // Nothing unrelated removed (the app-side lsp.nextDiagnostic is not a plugin contribution, so
    // it never appears in the registry command ids to begin with).
    for id in before.iter().filter(|id| !id.starts_with("lsp.")) {
        assert!(
            reduced.command_ids().any(|c| &c == id),
            "disabling lsp wrongly removed unrelated command `{id}`"
        );
    }
}

#[test]
fn completion_contributes_through_the_public_api() {
    let reg = Registry::with_plugins(all_builtins());
    assert!(
        reg.command_ids().any(|id| id == COMPLETION_COMMAND),
        "completion's lsp.completion missing — is the widget wired as a plugin?"
    );
    let reduced = Registry::with_plugins(
        all_builtins()
            .into_iter()
            .filter(|p| p.id() != COMPLETION_ID),
    );
    assert!(
        !reduced.command_ids().any(|id| id == COMPLETION_COMMAND),
        "lsp.completion still present after disabling completion — it is hardcoded"
    );
    // Disabling completion must leave the separate `lsp`/`diagnostics` plugins' commands intact.
    assert!(reduced.command_ids().any(|id| id == LSP_COMMAND));
    assert!(reduced.command_ids().any(|id| id == DIAGNOSTICS_COMMAND));
}

#[test]
fn clipboard_contributes_through_the_public_api() {
    let reg = Registry::with_plugins(all_builtins());
    assert!(
        reg.command_ids().any(|id| id == CLIPBOARD_COMMAND),
        "clipboard's edit.paste missing — is copy/cut/paste wired as a plugin?"
    );
    let reduced = Registry::with_plugins(
        all_builtins()
            .into_iter()
            .filter(|p| p.id() != CLIPBOARD_ID),
    );
    for id in ["edit.copy", "edit.cut", "edit.paste"] {
        assert!(
            !reduced.command_ids().any(|c| c == id),
            "`{id}` still present after disabling clipboard — it is hardcoded, not a plugin"
        );
    }
    // Disabling clipboard must leave unrelated plugins' commands intact.
    assert!(reduced.command_ids().any(|id| id == COMPLETION_COMMAND));
    assert!(reduced.command_ids().any(|id| id == EXPLORER_COMMAND));
}

#[test]
fn vim_contributes_through_the_public_api() {
    let reg = Registry::with_plugins(all_builtins());
    assert!(
        reg.command_ids().any(|id| id == VIM_COMMAND),
        "vim.toggle missing — is the modal layer wired as a plugin?"
    );
    let reduced = Registry::with_plugins(all_builtins().into_iter().filter(|p| p.id() != VIM_ID));
    for id in ["vim.enable", "vim.disable", "vim.toggle"] {
        assert!(
            !reduced.command_ids().any(|c| c == id),
            "`{id}` still present after disabling vim — it is hardcoded, not a plugin"
        );
    }
    // Disabling vim leaves unrelated plugins' commands intact.
    assert!(reduced.command_ids().any(|id| id == CLIPBOARD_COMMAND));
}

#[test]
fn diagnostics_contributes_through_the_public_api() {
    let reg = Registry::with_plugins(all_builtins());
    assert!(
        reg.command_ids().any(|id| id == DIAGNOSTICS_COMMAND),
        "diagnostic-nav command missing — is the diagnostics model wired as a plugin?"
    );
    // The `diagnostics` plugin owns the model but contributes no panel; disabling it must remove
    // its commands and leave the `lsp` plugin's request commands intact.
    let reduced = Registry::with_plugins(
        all_builtins()
            .into_iter()
            .filter(|p| p.id() != DIAGNOSTICS_ID),
    );
    assert!(!reduced.command_ids().any(|id| id == DIAGNOSTICS_COMMAND));
    assert!(
        reduced.command_ids().any(|id| id == LSP_COMMAND),
        "disabling diagnostics must not remove the separate `lsp` plugin's commands"
    );
}

#[test]
fn terminal_contributes_through_the_public_api() {
    let reg = Registry::with_plugins(all_builtins());
    assert!(
        reg.command_ids().any(|id| id == TERMINAL_COMMAND),
        "terminal command missing — is the terminal dock wired as a plugin?"
    );
}

#[test]
fn disabling_terminal_removes_only_its_contributions() {
    let full = Registry::with_plugins(all_builtins());
    let before: Vec<String> = full.command_ids().map(|s| s.to_string()).collect();

    let reduced =
        Registry::with_plugins(all_builtins().into_iter().filter(|p| p.id() != TERMINAL_ID));

    assert!(
        !reduced.command_ids().any(|id| id == TERMINAL_COMMAND),
        "terminal command still present after disabling — it is hardcoded, not a plugin"
    );
    for id in before.iter().filter(|id| !id.starts_with("terminal.")) {
        assert!(
            reduced.command_ids().any(|c| &c == id),
            "disabling terminal wrongly removed unrelated command `{id}`"
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
    assert!(
        bound(&full, "ctrl+f", "search.find"),
        "find's ctrl+f must be contributed through the registry, not the defaults table"
    );
    assert!(
        bound(&full, "ctrl+v", "edit.paste"),
        "clipboard's ctrl+v must be contributed through the registry, not the defaults table"
    );

    let no_clipboard = Registry::with_plugins(
        all_builtins()
            .into_iter()
            .filter(|p| p.id() != CLIPBOARD_ID),
    );
    assert!(
        !bound(&no_clipboard, "ctrl+v", "edit.paste"),
        "disabling clipboard must unbind ctrl+v — otherwise the binding is hardcoded"
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
