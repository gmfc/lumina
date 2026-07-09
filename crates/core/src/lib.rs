//! `editor_core` — the headless editing model.
//!
//! Rope-backed documents, a normalized multi-cursor selection set, a reversible
//! transaction/undo model, motions, and the pure coordinate-mapping functions.
//! This crate has **zero** terminal/UI dependencies and is unit-testable without a TTY
//! (CLAUDE.md invariant #7).

pub mod document;
pub mod edit;
pub mod history;
pub mod motion;
pub mod pairs;
pub mod selection;
pub mod transaction;
pub mod view;
pub mod workspace;

pub use document::{Document, Encoding, LineEnding, SyntaxEdit};
pub use history::{GroupBreak, History};
pub use motion::Motion;
pub use pairs::{PairTable, DEFAULT_PAIRS};
pub use selection::{Selection, Selections};
pub use transaction::{Change, Transaction};
pub use view::ViewState;
pub use workspace::{DocId, Workspace};
