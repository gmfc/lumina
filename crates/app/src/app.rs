//! The `App`: terminal lifecycle, the input loop, and the command dispatcher.
//!
//! `App` owns the plugin `Registry` and the `EditorState` as separate fields so dispatch
//! can split-borrow (`registry.dispatch_command(id, &mut self.editor)`).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::event::{self, Event as CtEvent, KeyEventKind, MouseButton, MouseEventKind};
use editor_core::view::{screen_to_char, PaneGeometry};
use editor_core::{edit, motion};
use editor_core::{Document, Selection};
use editor_plugin::{Host, Registry};
use ratatui::DefaultTerminal;

use crate::editor::{EditorState, Focus};
use crate::files;
use crate::input::Command;
use crate::keymap::{Chord, Keymap};
use crate::picker::PickerKind;
use crate::ui::{self, Regions};

/// Tracks click cadence for double/triple-click detection.
struct ClickState {
    at: Instant,
    char_pos: usize,
    count: u8,
}

pub struct App {
    pub editor: EditorState,
    pub registry: Registry,
    pub quit: bool,
    /// Last body height in rows (for PageUp/PageDown).
    pub page_height: usize,
    /// Screen regions from the last rendered frame (for mouse hit-testing).
    pub regions: Regions,
    /// The active color theme (syntax + chrome).
    pub theme: crate::theme::Theme,
    /// The chord keymap (defaults + config overrides).
    pub keymap: crate::keymap::Keymap,
    /// Pending chord prefix (for multi-key chords like Ctrl+K Ctrl+S).
    pub pending: Vec<crate::keymap::Chord>,
    /// User configuration.
    pub config: crate::config::Config,
    /// System clipboard (with OSC 52 + internal-register fallbacks).
    clipboard: crate::clipboard::Clipboard,
    /// Char offset where the current drag began (selection anchor).
    drag_anchor: Option<usize>,
    /// Index of the tab currently being dragged to reorder, if any.
    tab_drag: Option<usize>,
    /// Last click for multi-click detection.
    last_click: Option<ClickState>,
    // --- Phase 8: background workers + external sync ---
    /// Sender handed to background workers (search, watcher). Bounded (see [`crate::worker`]).
    worker_tx: crate::worker::WorkerTx,
    /// Receiver drained each tick by the main loop.
    worker_rx: std::sync::mpsc::Receiver<crate::worker::WorkerMsg>,
    /// The filesystem debouncer; kept alive so the watch persists.
    _watcher: Option<Box<dyn std::any::Any>>,
    /// The user config file path, watched for hot-reload (plan §6).
    config_path: Option<PathBuf>,
    /// Content hashes of our own pending saves, to suppress save-echo (plan §6).
    pending_self_writes: std::collections::HashMap<PathBuf, u64>,
    /// Auto-scroll to the first externally-changed line on reload (follow mode).
    follow_mode: bool,
    // --- Phase 10: LSP ---
    /// Language-server manager (inert unless a server is configured).
    lsp: crate::lsp::LspManager,
    /// Last document revision sent to the LSP, per DocId (change debounce).
    lsp_sent_revision: std::collections::HashMap<editor_core::DocId, u64>,
    /// The bottom terminal dock (tabs of shell sessions).
    pub panel: crate::terminal::TerminalPanel,
    /// Paths of recently closed tabs, newest last — the "reopen closed editor" stack.
    closed_tabs: Vec<PathBuf>,
    /// The Settings tab's model + UI state, when a settings tab is open.
    pub settings: Option<crate::settings::SettingsView>,
    /// The `DocId` of the empty buffer backing the settings tab (so it lives in the
    /// normal tab machinery — switching, closing — while rendering a custom view).
    settings_doc: Option<editor_core::DocId>,
}

mod completion;
mod cursors;
mod diagnostics;
mod dispatch;
mod file_ops;
mod git;
mod keys;
mod lifecycle;
mod lsp;
mod mouse;
mod overlay;
mod palette;
mod panel;
mod settings;
mod vim;
mod workers;

/// Convert an LSP `(line, utf16_char)` position to a char offset in `doc`.
pub(crate) fn lsp_pos_to_char(doc: &Document, line: u32, char16: u32) -> usize {
    let line = (line as usize).min(doc.len_lines().saturating_sub(1));
    let text = doc.line_text(line);
    let text = text.trim_end_matches(['\n', '\r']);
    let col = editor_lsp::position::utf16_to_char_col(text, char16);
    doc.line_to_char(line) + col
}

/// True if screen cell `(col, row)` falls within `rect`.
fn in_rect(rect: ratatui::layout::Rect, col: u16, row: u16) -> bool {
    col >= rect.x && col < rect.x + rect.width && row >= rect.y && row < rect.y + rect.height
}

/// Snapshot every command a palette can run — the still-app-side `palette_entries` rows plus
/// every registry-contributed command — into a flat `CommandInfo` list. A palette plugin reads
/// this through `Host::commands`, mirroring the registry across the split-borrow wall (it sees
/// only `&mut EditorState`). Rebuilt whenever the plugin set changes (i.e. at construction).
fn command_catalog(registry: &Registry) -> Vec<editor_plugin::CommandInfo> {
    let mut cat: Vec<editor_plugin::CommandInfo> = crate::commands::palette_entries()
        .iter()
        .map(|(id, title)| editor_plugin::CommandInfo::new(*id, *title))
        .collect();
    for spec in registry.commands() {
        cat.push(editor_plugin::CommandInfo::new(
            spec.id.clone(),
            spec.title.clone(),
        ));
    }
    cat
}

/// Build the keymap from three tiers, later tiers overriding earlier ones:
/// built-in defaults < plugin-contributed bindings < user config. `Keymap::bind` is
/// last-writer-wins, so a user remap still overrides a plugin's chord, which in turn
/// overrides a default. Folding in `registry.keybindings()` is what lets a migrated
/// feature contribute its own chords (invariant #3) and honors an external plugin's
/// manifest bindings; see `crates/plugin/src/contribution.rs::KeybindingSpec`.
fn build_keymap(config: &crate::config::Config, registry: &Registry) -> Keymap {
    let mut km = Keymap::from_pairs(crate::commands::default_bindings().iter().copied());
    for kb in registry.keybindings() {
        km.bind(&kb.chord, &kb.command);
    }
    for (chord, id) in &config.keybindings {
        km.bind(chord, id);
    }
    km
}

#[cfg(test)]
mod tests;
