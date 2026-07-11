//! The fuzzy picker overlay — one component parameterized by its item source. It backs a
//! **unified** quick-open / command palette (VS Code style: files by default, commands when
//! the query starts with `>`), plus a Go-to-Line prompt and LSP location lists (plan §5).
//! A tiny built-in fuzzy matcher scores and ranks candidates.

/// What the picker is choosing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickerKind {
    /// Project files; `id` is the absolute path. The unified quick-open uses this as its base
    /// kind and carries `commands` too, switching to command mode on a leading `>`. (Goto-line is
    /// a generic prompt now, not a picker kind; LSP location lists are owned by the `lsp-nav`
    /// plugin, which opens a generic plugin-owned picker rather than a `PickerKind`.)
    File,
}

/// One selectable row.
#[derive(Debug, Clone)]
pub struct PickerItem {
    pub id: String,
    pub label: String,
}

pub struct Picker {
    pub kind: PickerKind,
    pub prompt: String,
    pub query: String,
    pub items: Vec<PickerItem>,
    /// The command source for the unified picker's `>` mode (empty for other kinds).
    pub commands: Vec<PickerItem>,
    /// Indices into the *active* source (`items` or `commands`) passing the filter, best first.
    pub filtered: Vec<usize>,
    pub selected: usize,
    /// When a plugin opened this picker (via `Host::open_picker`), the owning plugin id + the
    /// request token, so the app routes activation back to it. `None` for app-owned pickers
    /// (the LSP locations list).
    pub owner: Option<String>,
    pub token: Option<String>,
}

impl Picker {
    /// A unified quick-open / command palette: `files` by default, `commands` when the query
    /// starts with `>`. `start_in_commands` pre-fills the `>` (the command-palette entry point).
    pub fn unified(
        prompt: &str,
        files: Vec<PickerItem>,
        commands: Vec<PickerItem>,
        start_in_commands: bool,
    ) -> Picker {
        let mut p = Picker {
            kind: PickerKind::File,
            prompt: prompt.to_string(),
            query: if start_in_commands {
                ">".into()
            } else {
                String::new()
            },
            items: files,
            commands,
            filtered: Vec::new(),
            selected: 0,
            owner: None,
            token: None,
        };
        p.refilter();
        p
    }

    /// Tag this picker with the plugin that owns it (id + request token), so the app routes
    /// activation back through `Registry::activate_picker`.
    pub fn owned_by(mut self, owner: impl Into<String>, token: impl Into<String>) -> Picker {
        self.owner = Some(owner.into());
        self.token = Some(token.into());
        self
    }

    /// True when the `>` command mode is active (unified picker, query starts with `>`).
    pub fn command_mode(&self) -> bool {
        !self.commands.is_empty() && self.query.starts_with('>')
    }

    /// The source currently being filtered (files or commands).
    pub fn active_items(&self) -> &[PickerItem] {
        if self.command_mode() {
            &self.commands
        } else {
            &self.items
        }
    }

    /// The query used for matching, with the `>` command-mode sigil stripped.
    pub fn effective_query(&self) -> &str {
        if self.command_mode() {
            self.query.trim_start_matches('>').trim_start()
        } else {
            &self.query
        }
    }

    /// The title to show, reflecting the active mode.
    pub fn prompt_label(&self) -> &str {
        if self.command_mode() {
            "Command Palette"
        } else {
            &self.prompt
        }
    }

    /// Recompute the filtered/ranked list for the current query against the active source.
    pub fn refilter(&mut self) {
        let query = self.effective_query().to_string();
        let mut scored: Vec<(usize, i64)> = self
            .active_items()
            .iter()
            .enumerate()
            .filter_map(|(i, item)| fuzzy_score(&query, &item.label).map(|s| (i, s)))
            .collect();
        // Higher score first; ties keep original order (stable sort).
        scored.sort_by_key(|&(_, score)| std::cmp::Reverse(score));
        self.filtered = scored.into_iter().map(|(i, _)| i).collect();
        self.selected = 0;
    }

    pub fn selected_item(&self) -> Option<&PickerItem> {
        self.filtered
            .get(self.selected)
            .map(|&i| &self.active_items()[i])
    }

    pub fn move_selection(&mut self, delta: isize) {
        if self.filtered.is_empty() {
            return;
        }
        let n = self.filtered.len() as isize;
        self.selected = (self.selected as isize + delta).rem_euclid(n) as usize;
    }

    pub fn input_char(&mut self, ch: char) {
        self.query.push(ch);
        self.refilter();
    }

    pub fn backspace(&mut self) {
        self.query.pop();
        self.refilter();
    }
}

/// Subsequence fuzzy score: `None` if `query` isn't a subsequence of `text`; otherwise a
/// score rewarding contiguous runs and word-boundary matches (higher is better).
pub fn fuzzy_score(query: &str, text: &str) -> Option<i64> {
    if query.is_empty() {
        return Some(0);
    }
    let q: Vec<char> = query.chars().map(|c| c.to_ascii_lowercase()).collect();
    let t: Vec<char> = text.chars().collect();
    let mut qi = 0;
    let mut score = 0i64;
    let mut last: isize = -2;
    for (ti, &ch) in t.iter().enumerate() {
        if qi < q.len() && ch.to_ascii_lowercase() == q[qi] {
            score += if ti as isize == last + 1 { 10 } else { 1 };
            let word_start = ti == 0 || !t[ti - 1].is_alphanumeric();
            if word_start {
                score += 8;
            }
            last = ti as isize;
            qi += 1;
        }
    }
    if qi == q.len() {
        // Slight penalty for longer candidates so tighter matches rank first.
        Some(score - (t.len() as i64) / 8)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subsequence_required() {
        assert!(fuzzy_score("abc", "aXbXc").is_some());
        assert!(fuzzy_score("abc", "acb").is_none());
    }

    #[test]
    fn contiguous_and_word_start_rank_higher() {
        let contiguous = fuzzy_score("save", "File: Save").unwrap();
        let scattered = fuzzy_score("save", "s a v e scattered").unwrap();
        assert!(contiguous > scattered, "{contiguous} !> {scattered}");
    }

    #[test]
    fn picker_ranks_and_selects() {
        let items = vec![
            PickerItem {
                id: "1".into(),
                label: "Reload Window".into(),
            },
            PickerItem {
                id: "2".into(),
                label: "File: Save".into(),
            },
        ];
        let mut p = Picker::unified("Files", items, Vec::new(), false);
        p.query = "save".into();
        p.refilter();
        assert_eq!(p.selected_item().unwrap().id, "2");
    }

    fn item(id: &str, label: &str) -> PickerItem {
        PickerItem {
            id: id.into(),
            label: label.into(),
        }
    }

    #[test]
    fn unified_switches_to_commands_on_gt() {
        let files = vec![item("a.rs", "a.rs"), item("save.rs", "save.rs")];
        let commands = vec![item("file.save", "File: Save"), item("app.quit", "Quit")];
        let mut p = Picker::unified("Files", files, commands, false);
        // File mode by default.
        assert!(!p.command_mode());
        p.query = "save".into();
        p.refilter();
        assert_eq!(p.selected_item().unwrap().id, "save.rs");
        // A leading `>` flips to commands, and the sigil is stripped for matching.
        p.query = ">save".into();
        p.refilter();
        assert!(p.command_mode());
        assert_eq!(p.effective_query(), "save");
        assert_eq!(p.selected_item().unwrap().id, "file.save");
    }

    #[test]
    fn unified_can_start_in_command_mode() {
        let files = vec![item("a.rs", "a.rs")];
        let commands = vec![item("app.quit", "Quit")];
        let p = Picker::unified("Files", files, commands, true);
        assert!(p.command_mode());
        assert_eq!(p.prompt_label(), "Command Palette");
        assert_eq!(p.selected_item().unwrap().id, "app.quit");
    }
}
