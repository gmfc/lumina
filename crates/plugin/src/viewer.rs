//! What a file viewer publishes for its tab.
//!
//! A viewer renders through the *same* styled-row channel the explorer and search-results
//! panels already use ([`crate::PanelLine`] / [`crate::Span`]), just drawn full-pane instead of
//! in a dock. That is deliberate: it costs the plugin API no new rendering vocabulary, and the
//! theme resolves a viewer's style keys exactly as it resolves a panel's.
//!
//! Viewers are **read-only** by construction. There is no port here for writing bytes back, so
//! no viewer can turn into a second, unsupervised editing path around
//! [`editor_core::Transaction`] (invariant #1).

use crate::host::PanelLine;

/// The rendered content of a viewer tab.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ViewerContent {
    /// One-line summary drawn under the tab's header — page count, byte count, a truncation
    /// warning, or why nothing could be extracted. `None` draws no status row.
    pub status: Option<String>,
    /// The body, one [`PanelLine`] per row. The app owns scrolling; a viewer publishes the
    /// whole document and never sees the viewport.
    pub lines: Vec<PanelLine>,
}

impl ViewerContent {
    /// Content with a status line and no body — the shape of an "I can't read this" result.
    pub fn status_only(status: impl Into<String>) -> ViewerContent {
        ViewerContent {
            status: Some(status.into()),
            lines: Vec::new(),
        }
    }

    /// Content from plain text, one row per line. Convenience for the common case where a
    /// viewer has extracted text and wants no per-span styling.
    pub fn from_text(text: &str) -> ViewerContent {
        ViewerContent {
            status: None,
            lines: text
                .lines()
                .map(|l| PanelLine::new(vec![crate::host::Span::plain(l)]))
                .collect(),
        }
    }

    pub fn status(mut self, status: impl Into<String>) -> ViewerContent {
        self.status = Some(status.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_text_makes_one_row_per_line() {
        let c = ViewerContent::from_text("a\nb\n");
        assert_eq!(c.lines.len(), 2);
        assert_eq!(c.lines[0].spans[0].text, "a");
        assert!(c.status.is_none());
    }

    #[test]
    fn status_only_has_no_body() {
        let c = ViewerContent::status_only("encrypted");
        assert!(c.lines.is_empty());
        assert_eq!(c.status.as_deref(), Some("encrypted"));
    }
}
