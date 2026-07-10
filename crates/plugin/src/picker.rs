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
}

impl CommandInfo {
    pub fn new(id: impl Into<String>, title: impl Into<String>) -> Self {
        CommandInfo {
            id: id.into(),
            title: title.into(),
        }
    }
}

/// One selectable row: an opaque `id` handed back on activation, plus a display `label`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickerItem {
    pub id: String,
    pub label: String,
}

impl PickerItem {
    pub fn new(id: impl Into<String>, label: impl Into<String>) -> Self {
        PickerItem {
            id: id.into(),
            label: label.into(),
        }
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
