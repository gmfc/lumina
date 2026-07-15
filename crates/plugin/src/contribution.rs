//! Declarative contributions — the parts of a plugin readable *without* running it
//! (like VS Code's `package.json`). A native plugin returns these from
//! [`crate::Plugin::contributions`]; an external plugin declares them in `plugin.toml`.

/// A command: an id the palette/keymap can invoke, plus a human title.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSpec {
    pub id: String,
    pub title: String,
    /// Optional grouping category shown in the palette (e.g. "File", "Edit").
    pub category: Option<String>,
}

impl CommandSpec {
    pub fn new(id: impl Into<String>, title: impl Into<String>) -> Self {
        CommandSpec {
            id: id.into(),
            title: title.into(),
            category: None,
        }
    }

    pub fn category(mut self, c: impl Into<String>) -> Self {
        self.category = Some(c.into());
        self
    }
}

/// Where a panel lives in the chrome.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanelLocation {
    /// Left sidebar (explorer, search results).
    Sidebar,
    /// Bottom panel (search results, terminal-like output).
    Bottom,
}

/// A UI panel a plugin owns and renders into.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PanelSpec {
    pub id: String,
    pub title: String,
    pub location: PanelLocation,
}

impl PanelSpec {
    pub fn new(id: impl Into<String>, title: impl Into<String>, location: PanelLocation) -> Self {
        PanelSpec {
            id: id.into(),
            title: title.into(),
            location,
        }
    }
}

/// A status-bar item a plugin keeps updated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatusItemSpec {
    pub id: String,
    /// Lower priority sorts further left.
    pub priority: i32,
}

/// A key chord bound to a command id. `chord` is a human string like `"ctrl+s"` or
/// `"ctrl+k ctrl+s"` (space-separated chord sequence).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeybindingSpec {
    pub chord: String,
    pub command: String,
}

impl KeybindingSpec {
    pub fn new(chord: impl Into<String>, command: impl Into<String>) -> Self {
        KeybindingSpec {
            chord: chord.into(),
            command: command.into(),
        }
    }
}

/// A language association contributed by a plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanguageSpec {
    pub id: String,
    pub extensions: Vec<String>,
}

/// A theme contributed by a plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeSpec {
    pub id: String,
    pub name: String,
}

/// Where a context-menu item sits: groups render in this order, separated by a divider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum MenuGroup {
    Navigation,
    Refactor,
    Info,
    Edit,
}

/// The context predicate deciding whether a context-menu item is shown. Evaluated by the app at
/// menu-open time against the real editor state; an item whose predicate fails is hidden.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuWhen {
    /// Always shown (e.g. Paste).
    Always,
    /// A non-empty selection exists (Cut / Copy).
    HasSelection,
    /// A language server is running for the active document's language (Code Action / Format).
    LspEnabled,
    /// `LspEnabled` **and** the caret is on a symbol (Definition / References / Rename / Hover).
    LspOnWord,
}

/// A context-menu item a plugin contributes: a labelled entry that runs `command` when chosen,
/// placed in `group` and shown only when `when` holds. The command routes through the normal
/// dispatch path, so any command can become a menu entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuItemSpec {
    pub command: String,
    pub label: String,
    pub group: MenuGroup,
    pub when: MenuWhen,
}

impl MenuItemSpec {
    pub fn new(
        command: impl Into<String>,
        label: impl Into<String>,
        group: MenuGroup,
        when: MenuWhen,
    ) -> Self {
        MenuItemSpec {
            command: command.into(),
            label: label.into(),
            group,
            when,
        }
    }
}

/// The full declarative surface of a plugin.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Contributions {
    pub commands: Vec<CommandSpec>,
    pub panels: Vec<PanelSpec>,
    pub status_items: Vec<StatusItemSpec>,
    pub keybindings: Vec<KeybindingSpec>,
    pub languages: Vec<LanguageSpec>,
    pub themes: Vec<ThemeSpec>,
    pub menu_items: Vec<MenuItemSpec>,
}

impl Contributions {
    pub fn builder() -> ContributionsBuilder {
        ContributionsBuilder::default()
    }
}

/// Ergonomic builder for a plugin's contributions.
#[derive(Default)]
pub struct ContributionsBuilder {
    inner: Contributions,
}

impl ContributionsBuilder {
    pub fn command(mut self, id: impl Into<String>, title: impl Into<String>) -> Self {
        self.inner.commands.push(CommandSpec::new(id, title));
        self
    }

    pub fn command_spec(mut self, spec: CommandSpec) -> Self {
        self.inner.commands.push(spec);
        self
    }

    pub fn panel(
        mut self,
        id: impl Into<String>,
        title: impl Into<String>,
        location: PanelLocation,
    ) -> Self {
        self.inner.panels.push(PanelSpec::new(id, title, location));
        self
    }

    pub fn status_item(mut self, id: impl Into<String>, priority: i32) -> Self {
        self.inner.status_items.push(StatusItemSpec {
            id: id.into(),
            priority,
        });
        self
    }

    pub fn keybinding(mut self, chord: impl Into<String>, command: impl Into<String>) -> Self {
        self.inner
            .keybindings
            .push(KeybindingSpec::new(chord, command));
        self
    }

    pub fn language(mut self, id: impl Into<String>, extensions: &[&str]) -> Self {
        self.inner.languages.push(LanguageSpec {
            id: id.into(),
            extensions: extensions.iter().map(|s| s.to_string()).collect(),
        });
        self
    }

    /// Contribute a context-menu item that runs `command` (in `group`, shown when `when` holds).
    pub fn menu_item(
        mut self,
        command: impl Into<String>,
        label: impl Into<String>,
        group: MenuGroup,
        when: MenuWhen,
    ) -> Self {
        self.inner
            .menu_items
            .push(MenuItemSpec::new(command, label, group, when));
        self
    }

    pub fn build(self) -> Contributions {
        self.inner
    }
}
