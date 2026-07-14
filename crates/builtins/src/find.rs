//! In-file find/replace, implemented **as a plugin** (invariant #3).
//!
//! The plugin owns the entire find model ([`FindState`], regex-backed with case / whole-word /
//! regex toggles and `$1` capture references) and reaches the editor only through [`Host`]: it
//! publishes the match highlight as a `"find.match"` decoration layer, moves the caret to the
//! current match with [`Host::set_selections`], edits via [`Host::apply_transaction`], and drives
//! its UI through the generic [`Prompt`] port ([`Host::set_prompt`]). While the prompt is up the
//! app routes raw keys to [`Plugin::on_prompt_key`]; nothing here touches ratatui or the rope.

use editor_core::{Change, DocId, Selection, Selections, Transaction};
use editor_plugin::{
    Contributions, Decoration, DecorationSet, Event, Host, Key, KeyCode, Plugin, Prompt,
    PromptField, PromptPlacement, PromptToggle,
};
use regex::{Regex, RegexBuilder};

/// Upper bound on in-file matches tracked at once. A broad query (e.g. `.`) on a large file
/// could otherwise produce a match-per-char list that bloats memory and the per-frame highlight
/// scan; capping keeps find responsive (project search caps similarly).
const MAX_MATCHES: usize = 5000;

/// The decoration layer key the plugin publishes match highlights under.
const FIND_LAYER: &str = "find.match";

/// Which field of the find widget currently receives typed input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Query,
    Replace,
}

/// The find/replace model: query, replacement, toggles, and the current match set. Pure over a
/// `&str` (only depends on `regex`), so it unit-tests without an editor.
pub struct FindState {
    pub query: String,
    pub replace: String,
    /// Whether the replace row is shown (Ctrl+H vs Ctrl+F).
    pub replace_mode: bool,
    pub field: Field,
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub regex: bool,
    /// All match char ranges `[start, end)`, in document order.
    pub matches: Vec<(usize, usize)>,
    /// Index of the "current" match within `matches`.
    pub current: usize,
    /// Char offset the widget opened at — the stable anchor for "nearest match" while typing
    /// (so the current match doesn't drift as the query grows).
    pub origin: usize,
    /// Set when the regex fails to compile.
    pub error: Option<String>,
}

impl FindState {
    pub fn new(replace_mode: bool) -> FindState {
        FindState {
            query: String::new(),
            replace: String::new(),
            replace_mode,
            field: Field::Query,
            case_sensitive: false,
            whole_word: false,
            regex: false,
            matches: Vec::new(),
            current: 0,
            origin: 0,
            error: None,
        }
    }

    /// Build the effective regex from the query + toggles.
    fn build(&self) -> Result<Regex, String> {
        let base = if self.regex {
            self.query.clone()
        } else {
            regex::escape(&self.query)
        };
        let pattern = if self.whole_word {
            format!(r"\b(?:{base})\b")
        } else {
            base
        };
        RegexBuilder::new(&pattern)
            .case_insensitive(!self.case_sensitive)
            .build()
            .map_err(|e| e.to_string())
    }

    /// Recompute matches over `text`, keeping the current match near `cursor_char`.
    pub fn recompute(&mut self, text: &str, cursor_char: usize) {
        self.matches.clear();
        self.error = None;
        if self.query.is_empty() {
            return;
        }
        let re = match self.build() {
            Ok(re) => re,
            Err(e) => {
                self.error = Some(e);
                return;
            }
        };
        // Collect match byte ranges, capped so a pathological query on a huge file (e.g. `.`)
        // can't produce a match-per-char list that blows up memory and the per-frame highlight
        // scan. Matches arrive in ascending, non-overlapping order.
        let mut raw: Vec<(usize, usize)> = Vec::new();
        for m in re.find_iter(text) {
            if m.start() == m.end() {
                continue; // skip empty matches
            }
            raw.push((m.start(), m.end()));
            if raw.len() >= MAX_MATCHES {
                break;
            }
        }

        // Convert the ascending byte boundaries to char offsets in a single pass — O(matches)
        // space, rather than materializing a whole-document byte→char table on every keystroke.
        let mut targets: Vec<usize> = Vec::with_capacity(raw.len() * 2);
        for &(s, e) in &raw {
            targets.push(s);
            targets.push(e);
        }
        let mut char_of_target = vec![0usize; targets.len()];
        let mut ti = 0;
        let mut char_idx = 0usize;
        for (b, _) in text.char_indices() {
            while ti < targets.len() && targets[ti] <= b {
                char_of_target[ti] = char_idx;
                ti += 1;
            }
            char_idx += 1;
        }
        // Any remaining targets sit at end-of-text (byte == text.len()).
        while ti < targets.len() {
            char_of_target[ti] = char_idx;
            ti += 1;
        }
        for (i, _) in raw.iter().enumerate() {
            self.matches
                .push((char_of_target[i * 2], char_of_target[i * 2 + 1]));
        }
        // Current = first match at/after the cursor, else the last.
        self.current = self
            .matches
            .iter()
            .position(|&(s, _)| s >= cursor_char)
            .unwrap_or(0);
    }

    pub fn current_match(&self) -> Option<(usize, usize)> {
        self.matches.get(self.current).copied()
    }

    pub fn select_next(&mut self) {
        if !self.matches.is_empty() {
            self.current = (self.current + 1) % self.matches.len();
        }
    }

    pub fn select_prev(&mut self) {
        if !self.matches.is_empty() {
            self.current = (self.current + self.matches.len() - 1) % self.matches.len();
        }
    }

    /// The compiled effective regex, for callers that reuse it across many matches (Replace All)
    /// instead of rebuilding it per replacement. `None` when the pattern fails to compile.
    pub fn compiled(&self) -> Option<Regex> {
        self.build().ok()
    }

    /// The replacement string for a match's captured text (expands `$1` etc. in regex mode).
    pub fn replacement_for(&self, matched: &str) -> String {
        self.replacement_with(self.compiled().as_ref(), matched)
    }

    /// Like [`Self::replacement_for`] but uses a pre-built regex, so a bulk replace compiles the
    /// pattern once rather than once per match. `re` is ignored outside regex mode.
    pub fn replacement_with(&self, re: Option<&Regex>, matched: &str) -> String {
        if !self.regex {
            return self.replace.clone();
        }
        match re {
            Some(re) => {
                if let Some(caps) = re.captures(matched) {
                    let mut out = String::new();
                    caps.expand(&self.replace, &mut out);
                    out
                } else {
                    self.replace.clone()
                }
            }
            None => self.replace.clone(),
        }
    }

    /// Type a char into the focused field.
    pub fn input_char(&mut self, ch: char) {
        match self.field {
            Field::Query => self.query.push(ch),
            Field::Replace => self.replace.push(ch),
        }
    }

    /// Backspace the focused field.
    pub fn backspace(&mut self) {
        match self.field {
            Field::Query => {
                self.query.pop();
            }
            Field::Replace => {
                self.replace.pop();
            }
        }
    }

    /// Switch focus between query and replace (only meaningful in replace mode).
    pub fn toggle_field(&mut self) {
        if self.replace_mode {
            self.field = match self.field {
                Field::Query => Field::Replace,
                Field::Replace => Field::Query,
            };
        }
    }

    /// Render the model into the generic [`Prompt`] the app draws (top-right find widget).
    fn to_prompt(&self) -> Prompt {
        let mut fields = vec![PromptField::new("Find", self.query.clone())];
        if self.replace_mode {
            fields.push(PromptField::new("Repl", self.replace.clone()));
        }
        let focused = match self.field {
            Field::Query => 0,
            Field::Replace => 1,
        };
        let status = if self.error.is_some() {
            "err".to_string()
        } else if self.matches.is_empty() {
            "0/0".to_string()
        } else {
            format!("{}/{}", self.current + 1, self.matches.len())
        };
        Prompt {
            owner: FindReplacePlugin::ID.to_string(),
            prompt_id: FindReplacePlugin::ID.to_string(),
            title: None,
            fields,
            focused,
            toggles: vec![
                PromptToggle::new("Aa", self.case_sensitive),
                PromptToggle::new("W", self.whole_word),
                PromptToggle::new(".*", self.regex),
            ],
            status: Some(status),
            footer: None,
            error: self.error.clone(),
            placement: PromptPlacement::TopRight,
        }
    }
}

/// The find/replace feature as a plugin. Owns the [`FindState`] while the widget is open.
#[derive(Default)]
pub struct FindReplacePlugin {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literal_matches() {
        let mut f = FindState::new(false);
        f.query = "lo".into();
        f.recompute("hello world lo", 0);
        assert_eq!(f.matches, vec![(3, 5), (12, 14)]);
    }

    #[test]
    fn case_and_word_toggles() {
        let mut f = FindState::new(false);
        f.query = "the".into();
        f.recompute("The theme is theirs", 0);
        assert_eq!(f.matches.len(), 3); // case-insensitive by default
        f.case_sensitive = true;
        f.recompute("The theme is theirs", 0);
        assert_eq!(f.matches.len(), 2); // "The" excluded
        f.whole_word = true;
        f.recompute("The theme is theirs the", 0);
        assert_eq!(f.matches.len(), 1); // only standalone "the"
    }

    #[test]
    fn regex_capture_replacement() {
        let mut f = FindState::new(true);
        f.regex = true;
        f.query = r"(\w+)@(\w+)".into();
        f.replace = "$2.$1".into();
        assert_eq!(f.replacement_for("user@host"), "host.user");
    }

    #[test]
    fn replacement_with_prebuilt_regex_matches_replacement_for() {
        // Replace All compiles the regex once and reuses it; the result must equal the per-call
        // path. Also covers the non-regex short-circuit and the `None` (no regex) branch.
        let mut f = FindState::new(true);
        f.regex = true;
        f.query = r"(\w+)@(\w+)".into();
        f.replace = "$2.$1".into();
        let re = f.compiled();
        assert_eq!(f.replacement_with(re.as_ref(), "user@host"), "host.user");
        // A None regex in regex mode falls back to the literal replacement.
        assert_eq!(f.replacement_with(None, "user@host"), "$2.$1");
        // Outside regex mode the replacement is verbatim regardless of the passed regex.
        f.regex = false;
        f.replace = "PLAIN".into();
        assert_eq!(
            f.replacement_with(f.compiled().as_ref(), "whatever"),
            "PLAIN"
        );
    }

    #[test]
    fn cursor_selects_nearest_forward_match() {
        let mut f = FindState::new(false);
        f.query = "x".into();
        f.recompute("x..x..x", 3);
        assert_eq!(f.current_match(), Some((3, 4)));
    }

    #[test]
    fn multibyte_offsets_are_char_ranges_not_byte_ranges() {
        // "é" is two bytes; matches must be reported in char offsets. "café" then "café".
        let mut f = FindState::new(false);
        f.query = "café".into();
        f.recompute("café café", 0);
        // char offsets: first "café" = [0,4); space at 4; second "café" = [5,9).
        assert_eq!(f.matches, vec![(0, 4), (5, 9)]);
    }

    #[test]
    fn match_count_is_capped() {
        let mut f = FindState::new(false);
        f.regex = true;
        f.query = ".".into(); // matches every char
        let text = "a".repeat(MAX_MATCHES + 500);
        f.recompute(&text, 0);
        assert_eq!(f.matches.len(), MAX_MATCHES);
    }

    #[test]
    fn to_prompt_mirrors_state_and_count() {
        let mut f = FindState::new(true);
        f.query = "x".into();
        f.regex = true;
        f.recompute("x x x", 0);
        let p = f.to_prompt();
        assert_eq!(p.owner, "find");
        assert_eq!(p.fields.len(), 2); // Find + Repl in replace mode
        assert_eq!(p.status.as_deref(), Some("1/3"));
        assert!(p.toggles.iter().any(|t| t.label == ".*" && t.on));
    }
}
