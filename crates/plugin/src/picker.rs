//! `editor_plugin::picker` — the generic fuzzy-list overlay port.
//!
//! A plugin describes a picker ([`PickerRequest`]) and publishes it via [`crate::Host::open_picker`];
//! the app owns the fuzzy filter, rendering, and key capture, then routes the chosen row back to
//! the owner's [`crate::Plugin::on_picker_activate`]. A palette plugin also enumerates every
//! command through [`crate::Host::commands`] and the project's files through
//! [`crate::Host::project_files`], so it needs neither the registry (unreachable through `Host`)
//! nor the `ignore` crate.

/// A command mirrored onto the host so a palette plugin can enumerate every command (built-in +
/// contributed) without reaching the registry, which is mid-dispatch behind the split-borrow wall.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandInfo {
    pub id: String,
    pub title: String,
    /// The chord currently bound to this command (`"Ctrl+K Ctrl+S"`), resolved from the *live*
    /// keymap by the host — so it reflects plugin contributions and user `[keys]` overrides, and
    /// is `None` for a command with no chord. The palette shows it, which is how a user learns a
    /// shortcut for something they just ran by name.
    pub keys: Option<String>,
}

impl CommandInfo {
    pub fn new(id: impl Into<String>, title: impl Into<String>) -> Self {
        CommandInfo {
            id: id.into(),
            title: title.into(),
            keys: None,
        }
    }

    /// Attach the command's current chord.
    pub fn keys(mut self, keys: Option<String>) -> Self {
        self.keys = keys;
        self
    }
}

/// One selectable row: an opaque `id` handed back on activation, plus a display `label`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickerItem {
    pub id: String,
    pub label: String,
    /// Trailing, right-aligned annotation for the row — a keybinding on a command row. Not part
    /// of the fuzzy-match text, so typing a chord doesn't filter by it.
    pub hint: Option<String>,
}

impl PickerItem {
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        PickerItem {
            id: id.into(),
            label: label.into(),
            hint: None,
        }
    }

    /// Annotate the row (e.g. with its keybinding).
    pub fn hint(mut self, hint: Option<String>) -> Self {
        self.hint = hint;
        self
    }
}

/// A request to open the app's generic fuzzy picker. Carries the owning plugin id + a `token`
/// (which of the owner's pickers this is) so activation routes back correctly. `items` is the
/// base source (e.g. files); `commands` is the optional secondary source reached with a leading
/// `>` (the unified quick-open ⇄ command-palette switch). `start_in_commands` opens directly in
/// the command view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickerRequest {
    pub owner: String,
    pub token: String,
    pub title: String,
    pub items: Vec<PickerItem>,
    pub commands: Vec<PickerItem>,
    pub start_in_commands: bool,
}
