//! In-file find/replace, implemented **as a plugin** (invariant #3).
//!
//! The plugin owns the entire find model ([`FindState`], regex-backed with case / whole-word /
//! regex toggles and `$1` capture references) and reaches the editor only through [`Host`]: it
//! publishes the match highlight as a `"find.match"` decoration layer, moves the caret to the
//! current match with [`Host::set_selections`], edits via [`Host::apply_transaction`], and drives
//! its UI through the generic [`Prompt`] port ([`Host::set_prompt`]). While the prompt is up the
//! app routes raw keys to [`Plugin::on_prompt_key`]; nothing here touches ratatui or the rope.

use editor_core::{Change, DocId, Selection, Selections, Transaction};
use editor_plugin::{Contributions, Decoration, DecorationSet, Event, Host, Key, KeyCode, Plugin};

mod state;
use state::FindState;

/// The decoration layer key the plugin publishes match highlights under.
const FIND_LAYER: &str = "find.match";

/// The find/replace feature as a plugin. Owns the [`FindState`] while the widget is open.
#[derive(Default)]
pub(crate) struct FindReplacePlugin {
    state: Option<FindState>,
}

impl FindReplacePlugin {
    const ID: &'static str = "find";

    /// Open the find (or find+replace) widget, seeding the query from the current selection and
    /// anchoring "nearest match" at the caret, then run a first search + publish the UI.
    fn open(&mut self, replace_mode: bool, host: &mut dyn Host) {
        let mut fs = FindState::new(replace_mode);
        if let Some(id) = host.active_doc() {
            if let Some(doc) = host.workspace().documents.get(id) {
                let sel = doc.selections.primary();
                fs.origin = sel.from();
                if !sel.is_empty() {
                    fs.query = doc.rope().slice(sel.from()..sel.to()).to_string();
                }
            }
        }
        self.state = Some(fs);
        self.refresh(host);
    }

    /// Close the widget: drop the state, dismiss the prompt, and clear the match highlight.
    fn close(&mut self, host: &mut dyn Host) {
        self.state = None;
        host.dismiss_prompt();
        if let Some(id) = host.active_doc() {
            host.clear_decorations(id, FIND_LAYER);
        }
    }

    /// Recompute matches against the active doc, move the caret to the current match, then
    /// re-publish the decoration layer + the prompt. The single "state changed" refresh.
    fn refresh(&mut self, host: &mut dyn Host) {
        let Some(id) = host.active_doc() else {
            return;
        };
        let Some(text) = host
            .workspace()
            .documents
            .get(id)
            .map(|d| d.rope().to_string())
        else {
            return;
        };
        if let Some(state) = self.state.as_mut() {
            let origin = state.origin;
            state.recompute(&text, origin);
        }
        self.focus(host, id);
        self.publish(host, id);
    }

    /// Move the caret to the current match so it scrolls into view (and shows the selection tint).
    fn focus(&self, host: &mut dyn Host, id: DocId) {
        if let Some((s, e)) = self.state.as_ref().and_then(|f| f.current_match()) {
            host.set_selections(id, Selections::single(Selection::new(s, e)));
        }
    }

    /// Publish the match-highlight decoration layer + the prompt from the current state, without
    /// recomputing or moving the caret (used after navigation / field switches).
    fn publish(&self, host: &mut dyn Host, id: DocId) {
        let Some(state) = self.state.as_ref() else {
            return;
        };
        let spans: Vec<Decoration> = state
            .matches
            .iter()
            .map(|&(s, e)| Decoration::new((s, e), FIND_LAYER))
            .collect();
        host.set_decorations(id, FIND_LAYER, DecorationSet::spans(spans));
        host.set_prompt(state.to_prompt());
    }

    /// Step the current match (next/prev), keep the caret on it, and re-publish.
    /// Apply `f` to the find state (if any), then recompute matches — the shared body of the
    /// option toggles + text edits driven from the find prompt.
    fn mutate_and_refresh(&mut self, host: &mut dyn Host, f: impl FnOnce(&mut FindState)) {
        if let Some(s) = self.state.as_mut() {
            f(s);
        }
        self.refresh(host);
    }

    /// Tab between the find and replace input fields, republishing the widget for the active doc.
    fn switch_field(&mut self, host: &mut dyn Host) {
        if let Some(s) = self.state.as_mut() {
            s.toggle_field();
        }
        if let Some(id) = host.active_doc() {
            self.publish(host, id);
        }
    }

    fn navigate(&mut self, host: &mut dyn Host, forward: bool) {
        let Some(id) = host.active_doc() else {
            return;
        };
        if let Some(state) = self.state.as_mut() {
            if forward {
                state.select_next();
            } else {
                state.select_prev();
            }
        }
        self.focus(host, id);
        self.publish(host, id);
    }

    /// Replace the current match with the (capture-expanded) replacement.
    fn replace_current(&mut self, host: &mut dyn Host) {
        let Some(id) = host.active_doc() else {
            return;
        };
        let Some((s, e)) = self.state.as_ref().and_then(|f| f.current_match()) else {
            return;
        };
        let txn = {
            let Some(doc) = host.workspace().documents.get(id) else {
                return;
            };
            // Defensive: a stale match (e.g. from a race with an external reload) could point
            // past the current buffer; skip rather than panic slicing out of range.
            if s > e || e > doc.len_chars() {
                return;
            }
            let matched = doc.rope().slice(s..e).to_string();
            let repl = self
                .state
                .as_ref()
                .map(|f| f.replacement_for(&matched))
                .unwrap_or_default();
            Transaction::replace(doc, s..e, &repl)
        };
        host.apply_transaction(id, txn);
        self.refresh(host);
    }

    /// Replace every match in one undoable transaction (plan §6).
    fn replace_all(&mut self, host: &mut dyn Host) {
        let Some(id) = host.active_doc() else {
            return;
        };
        let matches = self
            .state
            .as_ref()
            .map(|f| f.matches.clone())
            .unwrap_or_default();
        if matches.is_empty() {
            return;
        }
        // Compile the replacement regex once, not once per match: `replace_all` can touch up to
        // MAX_MATCHES (5000) hits, and rebuilding the pattern each time made a single Replace All
        // recompile the regex thousands of times.
        let re = self.state.as_ref().and_then(|f| f.compiled());
        let mut changes = Vec::with_capacity(matches.len());
        {
            let Some(doc) = host.workspace().documents.get(id) else {
                return;
            };
            let len = doc.len_chars();
            for &(s, e) in &matches {
                // Defensive: never slice past the current buffer (a stale match from a race
                // would otherwise panic ropey). Matches are normally kept fresh on reload.
                if s > e || e > len {
                    continue;
                }
                let matched = doc.rope().slice(s..e).to_string();
                let inserted = self
                    .state
                    .as_ref()
                    .map(|f| f.replacement_with(re.as_ref(), &matched))
                    .unwrap_or_default();
                changes.push(Change {
                    at: s,
                    removed: matched,
                    inserted,
                });
            }
        }
        let n = changes.len();
        host.apply_transaction(id, Transaction::from_changes(changes));
        host.notify(format!("Replaced {n} occurrence(s)"));
        self.refresh(host);
    }
}

impl Plugin for FindReplacePlugin {
    fn id(&self) -> &str {
        Self::ID
    }

    fn contributions(&self) -> Contributions {
        // Titles + chords travel with the plugin (invariant #3); the keymap folds in
        // registry-contributed bindings, so these `ctrl+f`/`ctrl+h`/`f3`/`shift+f3` rows left
        // `commands/tables.rs`.
        Contributions::builder()
            .command("search.find", "Find")
            .command("search.replace", "Replace")
            .command("search.findNext", "Find: Next Match")
            .command("search.findPrev", "Find: Previous Match")
            .command("search.replaceAll", "Replace: All")
            .keybinding("ctrl+f", "search.find")
            .keybinding("ctrl+h", "search.replace")
            .keybinding("f3", "search.findNext")
            .keybinding("shift+f3", "search.findPrev")
            .build()
    }

    fn run_command(&mut self, command_id: &str, host: &mut dyn Host) -> bool {
        match command_id {
            "search.find" => self.open(false, host),
            "search.replace" => self.open(true, host),
            "search.findNext" if self.state.is_some() => self.navigate(host, true),
            "search.findPrev" if self.state.is_some() => self.navigate(host, false),
            "search.replaceAll" => self.replace_all(host),
            // Still ours (next/prev with no open widget is a no-op), so claim it.
            "search.findNext" | "search.findPrev" => {}
            _ => return false,
        }
        true
    }

    fn on_prompt_key(&mut self, prompt_id: &str, key: Key, host: &mut dyn Host) -> bool {
        if prompt_id != Self::ID || self.state.is_none() {
            return false;
        }
        match key.code {
            KeyCode::Esc => self.close(host),
            KeyCode::Enter if key.alt => self.replace_current(host),
            KeyCode::Char('a' | 'A') if key.alt => self.replace_all(host),
            KeyCode::Char('c' | 'C') if key.alt => {
                self.mutate_and_refresh(host, |s| s.case_sensitive = !s.case_sensitive)
            }
            KeyCode::Char('w' | 'W') if key.alt => {
                self.mutate_and_refresh(host, |s| s.whole_word = !s.whole_word)
            }
            KeyCode::Char('r' | 'R') if key.alt => {
                self.mutate_and_refresh(host, |s| s.regex = !s.regex)
            }
            KeyCode::Up => self.navigate(host, false),
            KeyCode::Enter if key.shift => self.navigate(host, false),
            KeyCode::Enter | KeyCode::Down => self.navigate(host, true),
            KeyCode::Tab => self.switch_field(host),
            KeyCode::Backspace => self.mutate_and_refresh(host, |s| s.backspace()),
            KeyCode::Char(c) if !key.ctrl && !key.alt => {
                self.mutate_and_refresh(host, |s| s.input_char(c))
            }
            _ => {}
        }
        true
    }

    fn on_event(&mut self, event: &Event, host: &mut dyn Host) {
        if self.state.is_none() {
            return;
        }
        // Re-derive matches from fresh text after an edit or external reload, so a later replace
        // never slices with stale offsets. Recompute only — don't move the caret (mirrors the old
        // `refresh_find_after_reload`); our own replace already positioned it.
        let doc = match event {
            Event::DidChange(id) | Event::ExternalReload(id) => *id,
            _ => return,
        };
        if host.active_doc() != Some(doc) {
            return;
        }
        let Some(text) = host
            .workspace()
            .documents
            .get(doc)
            .map(|d| d.rope().to_string())
        else {
            return;
        };
        if let Some(state) = self.state.as_mut() {
            let origin = state.origin;
            state.recompute(&text, origin);
        }
        self.publish(host, doc);
    }
}
