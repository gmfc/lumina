//! Parsers that turn raw JSON-RPC response/notification payloads into this crate's models.
//!
//! Split out of [`super`] (the transport/handle machinery); the public parsers are re-exported
//! from there so external paths (`editor_lsp::client::parse_*`) are unchanged. This module is a
//! thin facade: the parsers live in per-concern submodules, grouped by feature, and each submodule
//! is re-exported here so the flat `parse_*` names stay in one namespace.

mod capabilities;
mod decorations;
mod diagnostics;
mod edits;
mod navigation;
mod shared;

pub use capabilities::*;
pub use decorations::*;
pub use diagnostics::*;
pub use edits::*;
pub use navigation::*;
