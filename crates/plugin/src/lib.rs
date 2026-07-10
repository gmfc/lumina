//! `editor_plugin` — the contribution API and plugin host (the kernel).
//!
//! One contribution model, two substrates (plan §6A): native in-tree plugins (the
//! `editor_builtins` crate) and, later, sandboxed external guests. Both drive the *same*
//! [`Registry`] and [`Host`] surface — there are no privileged back doors.
//!
//! The [`Registry`] owns the live plugin instances and aggregates their declarative
//! [`Contributions`]. The self-hosting test proves the built-ins reach the editor only
//! through this API.

pub mod contribution;
pub mod decoration;
pub mod event;
pub mod host;
pub mod input;
pub mod lsp;
pub mod overlay;
pub mod picker;
pub mod registry;
pub mod runtime;
pub mod wasm;

pub use contribution::CommandSpec;
pub use contribution::{
    Contributions, KeybindingSpec, LanguageSpec, PanelLocation, PanelSpec, StatusItemSpec,
    ThemeSpec,
};
pub use decoration::{Decoration, DecorationSet, GutterMark};
pub use event::Event;
pub use host::{Host, PanelContent, PanelLine, Span};
pub use input::{Key, KeyCode};
pub use lsp::LspRequestKind;
pub use overlay::{Prompt, PromptField, PromptPlacement, PromptToggle};
pub use picker::{CommandInfo, PickerItem, PickerRequest};
pub use registry::{Plugin, Registry};
