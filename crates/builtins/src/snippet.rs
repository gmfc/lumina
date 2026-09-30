//! Minimal LSP completion-snippet expansion (§5.2 grammar): `$1`, `${1:placeholder}`,
//! `${1|a,b,c|}` choice, `$0`, `${VAR}` / `${VAR:default}` variables, and `\$ \} \\ \,` escapes.
//!
//! Expands the snippet to plain text plus the tabstop ranges. On accept the completion plugin
//! inserts the text, starts a [`SnippetSession`], and places the caret on the first tabstop
//! (selecting its placeholder). Tab / Shift-Tab cycle stops; mirrored same-number placeholders
//! stay in sync while the session is active. Unknown variables resolve to their `:default` text
//! (or empty), never to a literal `$name`.

use editor_core::DocId;

/// A tabstop's number and its char range within the expanded [`Snippet::text`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Tabstop {
    pub(crate) number: u32,
    pub(crate) range: (usize, usize),
}

/// The result of expanding a snippet: plain text + tabstops.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Snippet {
    pub(crate) text: String,
    pub(crate) tabstops: Vec<Tabstop>,
}

impl Snippet {
    /// The tabstop the caret should land on after insertion: the lowest positive tabstop, else
    /// `$0`, else `None` (caret goes to the end of the inserted text).
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn first_stop(&self) -> Option<&Tabstop> {
        self.tabstops
            .iter()
            .filter(|t| t.number > 0)
            .min_by_key(|t| t.number)
            .or_else(|| self.tabstops.iter().find(|t| t.number == 0))
    }

    /// Unique tabstop numbers in visit order: ascending positives, then `$0` if present.
    pub(crate) fn visit_order(&self) -> Vec<u32> {
        let mut nums: Vec<u32> = self
            .tabstops
            .iter()
            .map(|t| t.number)
            .filter(|&n| n > 0)
            .collect();
        nums.sort_unstable();
        nums.dedup();
        if self.tabstops.iter().any(|t| t.number == 0) {
            nums.push(0);
        }
        nums
    }
}

/// An active multi-tabstop snippet session after accept.
#[derive(Debug, Clone)]
pub(crate) struct SnippetSession {
    pub(crate) doc: DocId,
    /// Absolute document ranges for each recorded tabstop (updated as the user edits).
    pub(crate) stops: Vec<Tabstop>,
    /// Visit order of tabstop numbers (positives ascending, then `$0`).
    pub(crate) order: Vec<u32>,
    /// Index into [`Self::order`] for the active tabstop group.
    pub(crate) index: usize,
}

impl SnippetSession {
    /// Build a session from an expanded snippet inserted at document char offset `base`.
    pub(crate) fn from_snippet(doc: DocId, base: usize, snip: &Snippet) -> Option<Self> {
        let order = snip.visit_order();
        if order.is_empty() {
            return None;
        }
        let stops: Vec<Tabstop> = snip
            .tabstops
            .iter()
            .map(|t| Tabstop {
                number: t.number,
                range: (base + t.range.0, base + t.range.1),
            })
            .collect();
        Some(SnippetSession {
            doc,
            stops,
            order,
            index: 0,
        })
    }

    pub(crate) fn active_number(&self) -> Option<u32> {
        self.order.get(self.index).copied()
    }

    /// Primary (first recorded) range for the active tabstop number.
    pub(crate) fn primary_range(&self) -> Option<(usize, usize)> {
        let n = self.active_number()?;
        self.stops.iter().find(|t| t.number == n).map(|t| t.range)
    }

    /// All absolute ranges sharing the active tabstop number (primary first).
    pub(crate) fn active_mirrors(&self) -> Vec<(usize, usize)> {
        let Some(n) = self.active_number() else {
            return Vec::new();
        };
        self.stops
            .iter()
            .filter(|t| t.number == n)
            .map(|t| t.range)
            .collect()
    }

    /// Advance to the next tabstop group. Returns `false` when the session should end (past `$0`
    /// or past the last stop).
    pub(crate) fn next(&mut self) -> bool {
        if self.index + 1 >= self.order.len() {
            return false;
        }
        self.index += 1;
        true
    }

    /// Move to the previous tabstop group. Stays put at the first.
    pub(crate) fn prev(&mut self) -> bool {
        if self.index == 0 {
            return true;
        }
        self.index -= 1;
        true
    }

    /// True when the caret lies inside any active-group range (or at a zero-width stop).
    pub(crate) fn caret_in_active(&self, head: usize) -> bool {
        self.active_mirrors().iter().any(|&(a, b)| {
            if a == b {
                head == a
            } else {
                head >= a && head <= b
            }
        })
    }

    /// Shift every stop range for an insertion/deletion of `delta` chars at `at`.
    /// Stops that start strictly after `at` move; stops that contain `at` grow/shrink their end.
    #[cfg(test)]
    pub(crate) fn shift_after(&mut self, at: usize, delta: isize) {
        for t in &mut self.stops {
            if t.range.0 > at {
                t.range.0 = add_delta(t.range.0, delta);
                t.range.1 = add_delta(t.range.1, delta);
            } else if t.range.1 >= at {
                // Edit landed at/inside this range — grow/shrink the end.
                t.range.1 = add_delta(t.range.1, delta);
            }
        }
    }

    /// Replace the absolute ranges for tabstop `number` with `ranges` (same length expected).
    pub(crate) fn set_ranges_for(&mut self, number: u32, ranges: &[(usize, usize)]) {
        let mut i = 0;
        for t in &mut self.stops {
            if t.number == number {
                if let Some(&r) = ranges.get(i) {
                    t.range = r;
                }
                i += 1;
            }
        }
    }
}

#[cfg(test)]
fn add_delta(pos: usize, delta: isize) -> usize {
    if delta >= 0 {
        pos.saturating_add(delta as usize)
    } else {
        pos.saturating_sub((-delta) as usize)
    }
}

/// Expand a snippet string.
pub(crate) fn expand(src: &str) -> Snippet {
    let chars: Vec<char> = src.chars().collect();
    let mut out = String::new();
    let mut stops = Vec::new();
    let mut i = 0;
    parse(&chars, &mut i, &mut out, &mut stops, false);
    Snippet {
        text: out,
        tabstops: stops,
    }
}

fn parse(chars: &[char], i: &mut usize, out: &mut String, stops: &mut Vec<Tabstop>, nested: bool) {
    while *i < chars.len() {
        let c = chars[*i];
        if nested && c == '}' {
            return; // caller consumes the closing brace
        }
        match c {
            '\\' => {
                *i += 1;
                if *i < chars.len() && matches!(chars[*i], '$' | '}' | '\\' | ',') {
                    out.push(chars[*i]);
                    *i += 1;
                } else {
                    out.push('\\');
                }
            }
            '$' => {
                *i += 1;
                parse_dollar(chars, i, out, stops);
            }
            _ => {
                out.push(c);
                *i += 1;
            }
        }
    }
}

fn parse_dollar(chars: &[char], i: &mut usize, out: &mut String, stops: &mut Vec<Tabstop>) {
    let Some(&c) = chars.get(*i) else {
        out.push('$'); // trailing `$`
        return;
    };
    match c {
        '{' => {
            *i += 1; // consume '{'
            parse_braced(chars, i, out, stops);
        }
        d if d.is_ascii_digit() => {
            // Bare `$1` — a zero-width tabstop at the current position.
            let num = read_number(chars, i);
            let at = out.chars().count();
            stops.push(Tabstop {
                number: num,
                range: (at, at),
            });
        }
        a if a.is_alphabetic() || a == '_' => skip_ident(chars, i), // bare $VAR → nothing
        _ => out.push('$'),
    }
}

/// Parse the body of a `${…}` construct (the `{` already consumed) and its closing `}`: either a
/// numbered tabstop (`${1}`, `${1:placeholder}`, `${1|a,b|}`) or a variable (`${VAR}`,
/// `${VAR:default}` — unknown vars fall back to their default or empty).
fn parse_braced(chars: &[char], i: &mut usize, out: &mut String, stops: &mut Vec<Tabstop>) {
    if chars.get(*i).is_some_and(|c| c.is_ascii_digit()) {
        parse_braced_tabstop(chars, i, out, stops);
    } else {
        skip_var_default(chars, i, out, stops);
    }
    consume_close(chars, i);
}

/// `${1:placeholder}` / `${1|a,b|}`: read the number, expand the placeholder (which may nest more
/// tabstops) or the first choice, and record the tabstop's span.
fn parse_braced_tabstop(chars: &[char], i: &mut usize, out: &mut String, stops: &mut Vec<Tabstop>) {
    let num = read_number(chars, i);
    let start = out.chars().count();
    match chars.get(*i) {
        Some(':') => {
            *i += 1;
            parse(chars, i, out, stops, true); // placeholder (may nest tabstops)
        }
        Some('|') => {
            *i += 1;
            read_choice_first(chars, i, out);
        }
        _ => {}
    }
    let end = out.chars().count();
    stops.push(Tabstop {
        number: num,
        range: (start, end),
    });
}

/// `${VAR}` / `${VAR:default}`: skip the (unknown) variable name, expanding its default if present.
fn skip_var_default(chars: &[char], i: &mut usize, out: &mut String, stops: &mut Vec<Tabstop>) {
    while *i < chars.len() && chars[*i] != '}' && chars[*i] != ':' {
        *i += 1;
    }
    if chars.get(*i) == Some(&':') {
        *i += 1;
        parse(chars, i, out, stops, true);
    }
}

/// Skip a bare `$VAR` identifier (alphanumerics + `_`).
fn skip_ident(chars: &[char], i: &mut usize) {
    while *i < chars.len() && (chars[*i].is_alphanumeric() || chars[*i] == '_') {
        *i += 1;
    }
}

fn read_number(chars: &[char], i: &mut usize) -> u32 {
    let mut n = String::new();
    while *i < chars.len() && chars[*i].is_ascii_digit() {
        n.push(chars[*i]);
        *i += 1;
    }
    n.parse().unwrap_or(0)
}

/// Emit the first choice option and skip the rest up to `}`.
fn read_choice_first(chars: &[char], i: &mut usize, out: &mut String) {
    while *i < chars.len() && !matches!(chars[*i], ',' | '|' | '}') {
        if chars[*i] == '\\' && *i + 1 < chars.len() {
            *i += 1;
        }
        out.push(chars[*i]);
        *i += 1;
    }
    while *i < chars.len() && chars[*i] != '}' {
        *i += 1;
    }
}

fn consume_close(chars: &[char], i: &mut usize) {
    if chars.get(*i) == Some(&'}') {
        *i += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use editor_core::{Document, Workspace};

    fn dummy_doc() -> editor_core::DocId {
        let mut ws = Workspace::new(std::path::PathBuf::from("/tmp"));
        ws.open_document(Document::from_str(""))
    }

    #[test]
    fn empty_tabstop_and_final_cursor() {
        let s = expand("println!($1)$0");
        assert_eq!(s.text, "println!()");
        // $1 at the '(' + 1 = char 9; $0 at end (10).
        assert_eq!(s.first_stop().unwrap().number, 1);
        assert_eq!(s.first_stop().unwrap().range, (9, 9));
        assert_eq!(s.visit_order(), vec![1, 0]);
    }

    #[test]
    fn placeholder_text_and_range() {
        let s = expand("for ${1:item} in ${2:iter} {\n\t$0\n}");
        assert!(s.text.starts_with("for item in iter {"));
        let t1 = s.first_stop().unwrap();
        assert_eq!(t1.number, 1);
        assert_eq!(&s.text[t1.range.0..t1.range.1], "item");
        assert_eq!(s.visit_order(), vec![1, 2, 0]);
    }

    #[test]
    fn choice_uses_first_and_vars_and_escapes() {
        assert_eq!(expand("${1|a,b,c|}").text, "a");
        assert_eq!(expand("${TM_UNKNOWN:def}").text, "def"); // unknown var → default
        assert_eq!(expand("$UNKNOWN").text, ""); // bare unknown var → empty
        assert_eq!(expand("cost is \\$5").text, "cost is $5"); // escaped $
    }

    #[test]
    fn mirrored_placeholders_share_a_number() {
        let s = expand("${1:x} = ${1:x}");
        assert_eq!(s.text, "x = x");
        let ones: Vec<_> = s.tabstops.iter().filter(|t| t.number == 1).collect();
        assert_eq!(ones.len(), 2);
        assert_eq!(&s.text[ones[0].range.0..ones[0].range.1], "x");
        assert_eq!(&s.text[ones[1].range.0..ones[1].range.1], "x");
    }

    #[test]
    fn session_cycles_and_ends_after_last() {
        let snip = expand("${1:a}${2:b}$0");
        let mut session = SnippetSession::from_snippet(dummy_doc(), 10, &snip).unwrap();
        assert_eq!(session.active_number(), Some(1));
        assert_eq!(session.primary_range(), Some((10, 11))); // "a" at base 10
        assert!(session.next());
        assert_eq!(session.active_number(), Some(2));
        assert!(session.next());
        assert_eq!(session.active_number(), Some(0));
        assert!(!session.next());
    }

    #[test]
    fn session_shift_after_adjusts_later_stops() {
        let snip = expand("${1:a}${2:b}");
        let mut session = SnippetSession::from_snippet(dummy_doc(), 0, &snip).unwrap();
        // Insert 2 chars inside stop 1 (at char 0) → end grows; later stop shifts.
        session.shift_after(0, 2);
        assert_eq!(session.stops[0].range, (0, 3)); // was (0,1), end grew
        assert_eq!(session.stops[1].range, (3, 4)); // was (1,2)
    }
}
