//! `editor_plugin::overlay` — a generic modal input widget a plugin describes and the app
//! renders on its behalf, so features like find/replace own no ratatui.
//!
//! A plugin publishes a [`Prompt`] via [`crate::Host::set_prompt`] and re-publishes it as its
//! state changes; the app draws it (a pure function of state, invariant #8) and forwards raw keys
//! to the owner's [`crate::Plugin::on_prompt_key`]. Nothing here carries a terminal/color type —
//! the app decides how a placement or a focused field looks.

/// One labelled text field in a prompt (e.g. find's query / replace rows).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptField {
    pub label: String,
    pub value: String,
}

impl PromptField {
    pub fn new(label: impl Into<String>, value: impl Into<String>) -> Self {
        PromptField {
            label: label.into(),
            value: value.into(),
        }
    }
}

/// A labelled on/off chip (e.g. find's case / whole-word / regex toggles).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptToggle {
    pub label: String,
    pub on: bool,
}

impl PromptToggle {
    pub fn new(label: impl Into<String>, on: bool) -> Self {
        PromptToggle {
            label: label.into(),
            on,
        }
    }
}

/// Where the app draws the prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromptPlacement {
    /// Centered box (rename / save-as / goto-line).
    Center,
    /// Top-right of the editor pane (find / replace).
    TopRight,
    /// The app draws nothing — the owner renders its own UI (e.g. in a panel) and uses the
    /// prompt only to receive keys while it is up (project search's query box).
    Panel,
}

/// A modal input widget owned by a plugin. The plugin re-publishes it as its state changes; the
/// app renders it and routes raw keys back to the owner via [`crate::Registry::dispatch_prompt_key`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Prompt {
    /// Owning plugin id — routes keys back to it while the prompt is up.
    pub owner: String,
    /// Which of the owner's prompts this is (a plugin may drive several).
    pub prompt_id: String,
    pub title: Option<String>,
    pub fields: Vec<PromptField>,
    /// Index into `fields` of the focused field.
    pub focused: usize,
    pub toggles: Vec<PromptToggle>,
    /// A short status shown beside the toggles (e.g. find's `"3/12"` match count).
    pub status: Option<String>,
    /// A footer hint line (e.g. `"[Enter] Apply   [Esc] Cancel"`).
    pub footer: Option<String>,
    /// An error line, shown emphasized.
    pub error: Option<String>,
    pub placement: PromptPlacement,
}

/// One row of a caret-anchored popup (a completion candidate, a hover line, …).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PopupRow {
    /// A short kind glyph/abbreviation shown before the label (e.g. "ƒ", "x").
    pub glyph: String,
    pub label: String,
    pub detail: Option<String>,
}

impl PopupRow {
    pub fn new(glyph: impl Into<String>, label: impl Into<String>, detail: Option<String>) -> Self {
        PopupRow {
            glyph: glyph.into(),
            label: label.into(),
            detail,
        }
    }
}

/// A caret-anchored floating list a plugin publishes (the completion popup today). The app owns
/// the on-screen positioning (`char_to_screen` on `anchor`, flip-above, scroll); the plugin
/// supplies only the anchor char offset + the rows + the selection, and receives navigation keys
/// via [`crate::Plugin::on_popup_key`] while it is up.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Popup {
    /// Owning plugin id — routes keys back to it while the popup is up.
    pub owner: String,
    /// Char offset the popup is anchored to (the start of the replaced identifier).
    pub anchor: usize,
    pub rows: Vec<PopupRow>,
    /// Highlighted row index into `rows`.
    pub selected: usize,
}

impl Prompt {
    /// An empty prompt owned by `owner`'s `prompt_id`, drawn at `placement`.
    pub fn new(
        owner: impl Into<String>,
        prompt_id: impl Into<String>,
        placement: PromptPlacement,
    ) -> Self {
        Prompt {
            owner: owner.into(),
            prompt_id: prompt_id.into(),
            title: None,
            fields: Vec::new(),
            focused: 0,
            toggles: Vec::new(),
            status: None,
            footer: None,
            error: None,
            placement,
        }
    }
}
