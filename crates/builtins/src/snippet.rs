//! Minimal LSP completion-snippet expansion (§5.2 grammar): `$1`, `${1:placeholder}`,
//! `${1|a,b,c|}` choice, `$0`, `${VAR}` / `${VAR:default}` variables, and `\$ \} \\ \,` escapes.
//!
//! Expands the snippet to plain text plus the tabstop ranges. On accept the completion plugin
//! inserts the text and places the caret at the first tabstop (selecting its placeholder). A full
//! multi-tabstop session (Tab/Shift-Tab cycling, mirrored edits) is a follow-up; unknown variables
//! resolve to their `:default` text (or empty), never to a literal `$name`.

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
    pub(crate) fn first_stop(&self) -> Option<&Tabstop> {
        self.tabstops
            .iter()
            .filter(|t| t.number > 0)
            .min_by_key(|t| t.number)
            .or_else(|| self.tabstops.iter().find(|t| t.number == 0))
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

    #[test]
    fn empty_tabstop_and_final_cursor() {
        let s = expand("println!($1)$0");
        assert_eq!(s.text, "println!()");
        // $1 at the '(' + 1 = char 9; $0 at end (10).
        assert_eq!(s.first_stop().unwrap().number, 1);
        assert_eq!(s.first_stop().unwrap().range, (9, 9));
    }

    #[test]
    fn placeholder_text_and_range() {
        let s = expand("for ${1:item} in ${2:iter} {\n\t$0\n}");
        assert!(s.text.starts_with("for item in iter {"));
        let t1 = s.first_stop().unwrap();
        assert_eq!(t1.number, 1);
        assert_eq!(&s.text[t1.range.0..t1.range.1], "item");
    }

    #[test]
    fn choice_uses_first_and_vars_and_escapes() {
        assert_eq!(expand("${1|a,b,c|}").text, "a");
        assert_eq!(expand("${TM_UNKNOWN:def}").text, "def"); // unknown var → default
        assert_eq!(expand("$UNKNOWN").text, ""); // bare unknown var → empty
        assert_eq!(expand("cost is \\$5").text, "cost is $5"); // escaped $
    }
}
