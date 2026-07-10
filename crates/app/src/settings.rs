//! The Settings editor model — a form of sections and typed widgets rendered as an
//! editor tab. The `App` owns the [`crate::config::Config`] (the source of truth);
//! this is a *view* rebuilt from it, plus the transient UI state (selection, scroll,
//! an in-progress text edit, an open dropdown). Changing a widget asks the `App` to
//! mutate the config, persist it, and rebuild the view (see `crate::app`'s `settings`
//! module), so the widgets always reflect the live config.

/// A typed control for one setting.
#[derive(Debug, Clone, PartialEq)]
pub enum Widget {
    /// A boolean checkbox.
    Toggle(bool),
    /// A one-of-many dropdown; `selected` indexes `options`.
    Select {
        options: Vec<String>,
        selected: usize,
    },
    /// An integer with `[min, max]` bounds (a stepper, also directly typeable).
    Number { value: i64, min: i64, max: i64 },
    /// A free-text field (empty string means "unset / default").
    Text(String),
}

/// One configurable setting.
#[derive(Debug, Clone)]
pub struct SettingItem {
    /// Stable key the `App` maps to a config field (e.g. `auto_pairs`, `plugin:explorer`).
    pub key: String,
    pub label: String,
    pub description: String,
    pub widget: Widget,
}

/// A rendered row: either a section header or a setting.
#[derive(Debug, Clone)]
pub enum Entry {
    Header(String),
    Item(SettingItem),
}

/// The Settings tab's model + UI state.
pub struct SettingsView {
    pub entries: Vec<Entry>,
    /// Index into `entries` of the focused item (always points at an `Item`).
    pub selected: usize,
    /// First visible entry row (vertical scroll).
    pub scroll: usize,
    /// The in-progress edit buffer for a `Text`/`Number` field, if editing.
    pub editing: Option<String>,
    /// When a `Select`'s dropdown is open, the highlighted option index.
    pub dropdown: Option<usize>,
}

impl SettingsView {
    /// Build the form from the current config and the list of `(plugin_id, enabled)`.
    pub fn build(config: &crate::config::Config, plugins: &[(String, bool)]) -> SettingsView {
        let mut entries = Vec::new();
        let section = |entries: &mut Vec<Entry>, title: &str| {
            entries.push(Entry::Header(title.to_string()));
        };
        let toggle = |entries: &mut Vec<Entry>, key: &str, label: &str, desc: &str, v: bool| {
            entries.push(Entry::Item(SettingItem {
                key: key.to_string(),
                label: label.to_string(),
                description: desc.to_string(),
                widget: Widget::Toggle(v),
            }));
        };

        section(&mut entries, "Editor");
        entries.push(Entry::Item(SettingItem {
            key: "tab_width".into(),
            label: "Tab width".into(),
            description: "Spaces per indentation level.".into(),
            widget: tab_width_select(config.tab_width),
        }));
        toggle(
            &mut entries,
            "auto_pairs",
            "Auto-close pairs",
            "Auto-close brackets/quotes, type over closers, delete empty pairs.",
            config.auto_pairs,
        );
        toggle(
            &mut entries,
            "auto_indent",
            "Auto-indent",
            "Copy indentation on newline; dedent on a closing bracket.",
            config.auto_indent,
        );

        section(&mut entries, "Files");
        toggle(
            &mut entries,
            "trim_trailing_whitespace",
            "Trim trailing whitespace",
            "Strip trailing spaces/tabs from every line on save.",
            config.trim_trailing_whitespace,
        );
        toggle(
            &mut entries,
            "insert_final_newline",
            "Insert final newline",
            "Ensure the file ends with a single newline on save.",
            config.insert_final_newline,
        );
        toggle(
            &mut entries,
            "follow_mode",
            "Follow external edits",
            "Auto-scroll to the first externally-changed line on reload.",
            config.follow_mode,
        );
        toggle(
            &mut entries,
            "poll_watch",
            "Poll for file changes",
            "Use polling instead of inotify (devcontainers / NFS mounts).",
            config.poll_watch,
        );

        section(&mut entries, "Interface");
        entries.push(Entry::Item(SettingItem {
            key: "sidebar_width".into(),
            label: "Sidebar width".into(),
            description: "Columns the explorer sidebar occupies.".into(),
            widget: Widget::Number {
                value: config.sidebar_width as i64,
                min: 10,
                max: 120,
            },
        }));
        toggle(
            &mut entries,
            "git_gutter",
            "Git gutter",
            "Per-line add/modify/delete change bar in the gutter.",
            config.git_gutter,
        );
        toggle(
            &mut entries,
            "icons",
            "File icons",
            "Nerd Font file-type glyphs in the explorer (needs a patched font).",
            config.icons,
        );

        section(&mut entries, "Terminal");
        entries.push(Entry::Item(SettingItem {
            key: "terminal_height".into(),
            label: "Terminal height".into(),
            description: "Rows the terminal panel occupies when expanded.".into(),
            widget: Widget::Number {
                value: config.terminal_height as i64,
                min: 3,
                max: 60,
            },
        }));
        entries.push(Entry::Item(SettingItem {
            key: "terminal_shell".into(),
            label: "Shell".into(),
            description: "Override the integrated-terminal shell (blank = platform default)."
                .into(),
            widget: Widget::Text(config.terminal_shell.clone().unwrap_or_default()),
        }));

        section(&mut entries, "Vim");
        toggle(
            &mut entries,
            "vim",
            "Vim mode",
            "Modal editing (Normal/Insert/Visual). Off = mouse-first editing.",
            config.vim,
        );

        if !plugins.is_empty() {
            section(&mut entries, "Plugins");
            for (id, enabled) in plugins {
                entries.push(Entry::Item(SettingItem {
                    key: format!("plugin:{id}"),
                    label: id.clone(),
                    description: "Enable this plugin (applied on the next launch).".into(),
                    widget: Widget::Toggle(*enabled),
                }));
            }
        }

        let selected = entries
            .iter()
            .position(|e| matches!(e, Entry::Item(_)))
            .unwrap_or(0);
        SettingsView {
            entries,
            selected,
            scroll: 0,
            editing: None,
            dropdown: None,
        }
    }

    pub fn selected_item(&self) -> Option<&SettingItem> {
        match self.entries.get(self.selected) {
            Some(Entry::Item(it)) => Some(it),
            _ => None,
        }
    }

    /// Move the selection to the next/previous *item* (skipping headers), clamped.
    pub fn move_selection(&mut self, delta: isize) {
        let n = self.entries.len();
        if n == 0 {
            return;
        }
        let mut i = self.selected as isize;
        loop {
            i += delta;
            if i < 0 || i >= n as isize {
                return; // hit an edge: keep the current selection
            }
            if matches!(self.entries[i as usize], Entry::Item(_)) {
                self.selected = i as usize;
                return;
            }
        }
    }
}

/// A `Select` of common tab widths, including the current value if unusual.
fn tab_width_select(current: usize) -> Widget {
    let mut options = vec![2usize, 4, 8];
    if !options.contains(&current) {
        options.push(current);
        options.sort_unstable();
    }
    let selected = options.iter().position(|&v| v == current).unwrap_or(0);
    Widget::Select {
        options: options.iter().map(|v| v.to_string()).collect(),
        selected,
    }
}
