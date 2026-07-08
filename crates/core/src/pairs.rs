//! Auto-closing pair logic (pure) — CLAUDE.md invariant #5: `editor-core` stays
//! terminal-free, and every decision here is computed from characters + immediate context,
//! never by poking the rope. The caller (in [`crate::edit`]) turns these intents into a
//! single [`crate::transaction::Transaction`] so undo, multi-cursor, and syntax-edit
//! tracking all keep working (plan §1.1).

/// The default, language-agnostic set of auto-closing pairs (plan §1.1). Quote-like pairs
/// have an identical open/close member.
pub const DEFAULT_PAIRS: &[(char, char)] = &[
    ('(', ')'),
    ('[', ']'),
    ('{', '}'),
    ('"', '"'),
    ('\'', '\''),
    ('`', '`'),
];

/// A set of `(open, close)` auto-closing pairs. Language-aware overrides can build a custom
/// table later (plan §1.1 "language-aware override"); for now every buffer uses
/// [`PairTable::default`].
#[derive(Debug, Clone)]
pub struct PairTable {
    pairs: Vec<(char, char)>,
}

impl Default for PairTable {
    fn default() -> Self {
        PairTable {
            pairs: DEFAULT_PAIRS.to_vec(),
        }
    }
}

impl PairTable {
    /// Build a table from explicit pairs.
    pub fn new(pairs: Vec<(char, char)>) -> Self {
        PairTable { pairs }
    }

    /// The closing partner for `open`, if `open` is an opening member of a pair. Quote-like
    /// pairs are their own partner.
    pub fn close_for(&self, open: char) -> Option<char> {
        self.pairs.iter().find(|(o, _)| *o == open).map(|(_, c)| *c)
    }

    /// True when `ch` opens a pair (includes quotes).
    pub fn is_open(&self, ch: char) -> bool {
        self.pairs.iter().any(|(o, _)| *o == ch)
    }

    /// True when `ch` closes a pair (includes quotes).
    pub fn is_close(&self, ch: char) -> bool {
        self.pairs.iter().any(|(_, c)| *c == ch)
    }

    /// True when `ch` is a symmetric quote-like pair (open == close).
    pub fn is_quote(&self, ch: char) -> bool {
        self.pairs.iter().any(|(o, c)| *o == ch && *o == *c)
    }

    /// True when `ch` is a *bracket* opener — a pair whose members differ (excludes quotes).
    pub fn is_open_bracket(&self, ch: char) -> bool {
        self.pairs.iter().any(|(o, c)| *o == ch && *o != *c)
    }

    /// True when `ch` is a *bracket* closer (excludes quotes).
    pub fn is_close_bracket(&self, ch: char) -> bool {
        self.pairs.iter().any(|(o, c)| *c == ch && *o != *c)
    }
}

/// What to do when a character is typed at a caret (plan §1.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InsertAction {
    /// Insert the typed char verbatim.
    Literal,
    /// Insert the typed opener plus this closing char; the caret lands between them.
    OpenPair(char),
    /// The matching closer already sits after the caret: step over it, inserting nothing.
    TypeOver,
}

/// Decide how a typed `ch` behaves given the chars immediately before/after a caret.
///
/// `prev`/`next` are the chars on either side of the caret (`None` at a buffer edge).
pub fn decide_insert(
    table: &PairTable,
    ch: char,
    prev: Option<char>,
    next: Option<char>,
) -> InsertAction {
    // Step over an existing closer (or quote) rather than duplicating it.
    if table.is_close(ch) && next == Some(ch) {
        return InsertAction::TypeOver;
    }
    if table.is_open(ch) {
        // Quotes: suppress auto-close directly after a word char or another same quote, so
        // `don` + `'` stays `don'` — never `don''` (plan §1.1).
        if table.is_quote(ch) {
            if let Some(p) = prev {
                if is_word_char(p) || p == ch {
                    return InsertAction::Literal;
                }
            }
        }
        if let Some(close) = table.close_for(ch) {
            return InsertAction::OpenPair(close);
        }
    }
    InsertAction::Literal
}

/// True when backspacing at a caret sitting exactly between an open/close pair should delete
/// both members (`(|)` → ``, plan §1.1).
pub fn is_empty_pair(table: &PairTable, prev: Option<char>, next: Option<char>) -> bool {
    match (prev, next) {
        (Some(p), Some(n)) => table.close_for(p) == Some(n),
        _ => false,
    }
}

/// A word char for the purpose of quote suppression.
fn is_word_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t() -> PairTable {
        PairTable::default()
    }

    #[test]
    fn open_bracket_auto_closes() {
        assert_eq!(
            decide_insert(&t(), '(', None, None),
            InsertAction::OpenPair(')')
        );
        assert_eq!(
            decide_insert(&t(), '{', Some('x'), None),
            InsertAction::OpenPair('}')
        );
    }

    #[test]
    fn typing_closer_over_existing_steps_past() {
        assert_eq!(
            decide_insert(&t(), ')', None, Some(')')),
            InsertAction::TypeOver
        );
        assert_eq!(
            decide_insert(&t(), '"', None, Some('"')),
            InsertAction::TypeOver
        );
    }

    #[test]
    fn closer_without_partner_ahead_is_literal() {
        // A `)` with something else after it is just a literal char.
        assert_eq!(
            decide_insert(&t(), ')', None, Some('x')),
            InsertAction::Literal
        );
    }

    #[test]
    fn quote_after_word_is_suppressed() {
        // `don` + `'` must not become `don''` (the apostrophe-in-contraction case).
        assert_eq!(
            decide_insert(&t(), '\'', Some('n'), None),
            InsertAction::Literal
        );
        assert_eq!(
            decide_insert(&t(), '\'', Some('_'), None),
            InsertAction::Literal
        );
        // But at a word boundary it still auto-closes.
        assert_eq!(
            decide_insert(&t(), '\'', Some(' '), None),
            InsertAction::OpenPair('\'')
        );
        assert_eq!(
            decide_insert(&t(), '"', None, None),
            InsertAction::OpenPair('"')
        );
    }

    #[test]
    fn empty_pair_detection() {
        assert!(is_empty_pair(&t(), Some('('), Some(')')));
        assert!(is_empty_pair(&t(), Some('"'), Some('"')));
        assert!(!is_empty_pair(&t(), Some('('), Some(']')));
        assert!(!is_empty_pair(&t(), Some('('), None));
        assert!(!is_empty_pair(&t(), None, Some(')')));
    }

    #[test]
    fn bracket_vs_quote_classification() {
        let t = t();
        assert!(t.is_open_bracket('{'));
        assert!(t.is_close_bracket('}'));
        assert!(!t.is_open_bracket('"'));
        assert!(!t.is_close_bracket('"'));
        assert!(t.is_quote('"'));
        assert!(!t.is_quote('{'));
    }
}
