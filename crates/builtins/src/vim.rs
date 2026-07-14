//! Vim modal editing, implemented **as a plugin** (invariant #3).
//!
//! The plugin owns the whole modal state machine ([`VimState`]) and intercepts keys through
//! [`Plugin::capture_key`] before chord resolution. Every buffer change is a `Transaction` applied
//! via [`Host::apply_transaction`] (+ [`Host::set_selections`] for the caret); the pure
//! motion/text-object math is `editor_core::vim`. App-level actions (undo/redo/save/close/quit) go
//! through [`Host::execute`]; page motions/recenter/goal-column vertical motion use the small
//! `viewport_height`/`move_lines`/`set_scroll` ports; clipboard registers use `clipboard_*`. The
//! mode + pending hint are mirrored to the app via [`Host::set_vim_view`] for the badge + visual
//! shading; the renderer never reaches into the plugin.

use editor_core::transaction::Change;
use editor_core::{DocId, Document, Selection, Selections, Transaction};
use editor_plugin::input::{Key, KeyCode};
use editor_plugin::{Contributions, Host, Plugin, VimMode, VimView};
use state::{Mode, VimState};

mod edit;
mod exline;
mod motion;
mod normal;
mod operators;
mod registers;
mod search;
mod state;
mod visual;

/// Toggle the case of a single char, as a `String` (case folding can widen).
fn toggle_case(c: char) -> String {
    if c.is_uppercase() {
        c.to_lowercase().collect()
    } else {
        c.to_uppercase().collect()
    }
}

/// Cap for `Change.at` so a bad range can't index past the buffer end.
fn removed_end_cap(host: &dyn Host, id: DocId) -> usize {
    host.workspace()
        .documents
        .get(id)
        .map(|d| d.len_chars())
        .unwrap_or(usize::MAX)
}

#[derive(Default)]
pub(crate) struct VimPlugin {
    /// `Some` while the vim layer is enabled; the modal state machine lives here.
    state: Option<VimState>,
}

impl VimPlugin {
    const ID: &'static str = "vim";

    fn s(&self) -> &VimState {
        self.state.as_ref().expect("vim enabled")
    }

    fn sm(&mut self) -> &mut VimState {
        self.state.as_mut().expect("vim enabled")
    }

    fn primary_head(host: &dyn Host) -> usize {
        host.active_doc()
            .and_then(|id| host.workspace().documents.get(id))
            .map(|d| d.selections.primary().head)
            .unwrap_or(0)
    }

    fn revision(host: &dyn Host) -> u64 {
        host.active_doc()
            .and_then(|id| host.workspace().documents.get(id))
            .map(|d| d.revision)
            .unwrap_or(0)
    }

    /// Read a value from the active document.
    fn read<T>(host: &dyn Host, f: impl FnOnce(&Document) -> T) -> Option<T> {
        let id = host.active_doc()?;
        host.workspace().documents.get(id).map(f)
    }

    /// Replace `[start, end)` in the active document with `text`, as one transaction.
    fn replace(host: &mut dyn Host, start: usize, end: usize, text: String) {
        let Some(id) = host.active_doc() else {
            return;
        };
        let removed = match host.workspace().documents.get(id) {
            Some(d) => {
                let (s, e) = (start.min(d.len_chars()), end.min(d.len_chars()));
                if s < e {
                    d.rope().slice(s..e).to_string()
                } else {
                    String::new()
                }
            }
            None => return,
        };
        let at = start.min(removed_end_cap(host, id));
        let txn = Transaction::from_changes(vec![Change {
            at,
            removed,
            inserted: text,
        }]);
        host.apply_transaction(id, txn);
    }

    /// Set the primary caret to `pos` (clamped), collapsing any selection.
    fn caret(host: &mut dyn Host, pos: usize) {
        let Some(id) = host.active_doc() else {
            return;
        };
        let p = match host.workspace().documents.get(id) {
            Some(d) => d.clamp(pos),
            None => return,
        };
        host.set_selections(id, Selections::single(Selection::caret(p)));
    }

    /// Set the primary selection to `[anchor, head]` (clamped head).
    fn select(host: &mut dyn Host, anchor: usize, head: usize) {
        let Some(id) = host.active_doc() else {
            return;
        };
        let h = match host.workspace().documents.get(id) {
            Some(d) => d.clamp(head),
            None => return,
        };
        host.set_selections(id, Selections::single(Selection::new(anchor, h)));
    }

    fn set_enabled(&mut self, on: bool, host: &mut dyn Host) {
        if on {
            if self.state.is_none() {
                self.state = Some(VimState::new());
            }
            host.notify("Vim mode enabled".into());
            self.publish(host);
        } else {
            self.state = None;
            host.notify("Vim mode disabled".into());
            host.set_vim_view(None);
        }
    }

    /// Publish the mode + pending hint for the status badge / visual shading.
    fn publish(&self, host: &mut dyn Host) {
        let Some(v) = self.state.as_ref() else {
            host.set_vim_view(None);
            return;
        };
        let mode = match v.mode {
            Mode::Normal => VimMode::Normal,
            Mode::Insert => VimMode::Insert,
            Mode::Visual => VimMode::Visual,
            Mode::VisualLine => VimMode::VisualLine,
        };
        host.set_vim_view(Some(VimView {
            mode,
            pending: v.pending_hint(),
        }));
    }

    /// Intercept a key. Returns `true` when Vim consumed it; `false` lets it fall through (so
    /// Insert-mode text still reaches the editor and global chords keep working).
    fn handle_key(&mut self, key: Key, host: &mut dyn Host) -> bool {
        if self.state.is_none() {
            return false;
        }
        let mode = self.s().mode;

        // The `:` command line and `/` search line are sub-modes that own the keyboard.
        if self.s().command.is_some() {
            self.command_key(key, host);
            self.publish(host);
            return true;
        }
        if self.s().search.is_some() {
            self.search_key(key, host);
            self.publish(host);
            return true;
        }

        // Record keys for `.` (dot-repeat), except while replaying and for `.` itself.
        let replaying = self.s().replaying;
        let is_dot = mode == Mode::Normal && key.code == KeyCode::Char('.') && self.s().is_idle();
        if !replaying && !is_dot {
            let rev = Self::revision(host);
            self.sm().record_key(key, rev);
        }

        let consumed = match mode {
            Mode::Insert => self.insert_key(key, host),
            Mode::Normal => self.normal_key(key, host),
            Mode::Visual | Mode::VisualLine => self.visual_key(key, host),
        };

        if !self.s().replaying {
            let rev = Self::revision(host);
            self.sm().finalize_recording(rev);
        }
        self.publish(host);
        consumed
    }
}

impl Plugin for VimPlugin {
    fn id(&self) -> &str {
        Self::ID
    }

    fn contributions(&self) -> Contributions {
        Contributions::builder()
            .command("vim.toggle", "Vim: Toggle Vim Mode")
            .command("vim.enable", "Vim: Enable Vim Mode")
            .command("vim.disable", "Vim: Disable Vim Mode")
            .build()
    }

    fn run_command(&mut self, command_id: &str, host: &mut dyn Host) -> bool {
        match command_id {
            "vim.enable" => self.set_enabled(true, host),
            "vim.disable" => self.set_enabled(false, host),
            "vim.toggle" => self.set_enabled(self.state.is_none(), host),
            _ => return false,
        }
        true
    }

    fn capture_key(&mut self, key: Key, host: &mut dyn Host) -> bool {
        self.handle_key(key, host)
    }
}
