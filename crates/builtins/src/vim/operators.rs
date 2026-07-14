//! Operators: delete/change/yank/indent/case over a motion range, line ops, and text objects.

use super::state::{Mode, MotionKind, Operator};
use super::toggle_case;
use super::VimPlugin;
use editor_core::transaction::Change;
use editor_core::vim as core_vim;
use editor_core::vim::TextObject;
use editor_core::{Document, Transaction};
use editor_plugin::Host;

/// The one indent/outdent [`Change`] for line `l`, or `None` when outdenting a line with no
/// leading indentation. Indent inserts four spaces at the line start; outdent removes one tab or
/// up to `width` leading spaces.
fn line_indent_change(d: &Document, l: usize, indent: bool, width: usize) -> Option<Change> {
    let ls = d.line_to_char(l);
    if indent {
        return Some(Change {
            at: ls,
            removed: String::new(),
            inserted: "    ".into(),
        });
    }
    let chars: Vec<char> = d.line_text(l).chars().collect();
    let remove = leading_indent_width(&chars, width);
    (remove > 0).then(|| Change {
        at: ls,
        removed: d.rope().slice(ls..ls + remove).to_string(),
        inserted: String::new(),
    })
}

/// Number of leading whitespace chars one outdent removes: a single tab, else up to `width` spaces.
fn leading_indent_width(chars: &[char], width: usize) -> usize {
    if chars.first() == Some(&'\t') {
        return 1;
    }
    let mut n = 0;
    while n < width && chars.get(n) == Some(&' ') {
        n += 1;
    }
    n
}

impl VimPlugin {
    pub(super) fn apply_operator_kind(
        &mut self,
        op: Operator,
        from: usize,
        to: usize,
        kind: MotionKind,
        host: &mut dyn Host,
    ) {
        let range = Self::read(host, |doc| {
            let (lo, hi) = (from.min(to), from.max(to));
            match kind {
                MotionKind::Exclusive => (lo, hi, false),
                MotionKind::Inclusive => (lo, (hi + 1).min(doc.len_chars()), false),
                MotionKind::Linewise => {
                    let fl = doc.char_to_line(lo);
                    let ll = doc.char_to_line(hi);
                    let s = doc.line_to_char(fl);
                    let e = if ll + 1 < doc.len_lines() {
                        doc.line_to_char(ll + 1)
                    } else {
                        doc.len_chars()
                    };
                    (s, e, true)
                }
            }
        });
        let Some((start, end, linewise)) = range else {
            return;
        };
        self.apply_operator_range(op, start, end, linewise, host);
    }

    pub(super) fn apply_operator_range(
        &mut self,
        op: Operator,
        start: usize,
        end: usize,
        linewise: bool,
        host: &mut dyn Host,
    ) {
        let reg = self.s().register;
        let text = Self::read(host, |d| {
            let (s, e) = (start.min(d.len_chars()), end.min(d.len_chars()));
            if s < e {
                d.rope().slice(s..e).to_string()
            } else {
                String::new()
            }
        })
        .unwrap_or_default();
        match op {
            Operator::Yank => {
                self.store_register(reg, text, linewise, true, host);
                Self::caret(host, start);
            }
            Operator::Delete => {
                self.store_register(reg, text, linewise, false, host);
                self.delete_range(start, end, linewise, host);
            }
            Operator::Change => {
                self.store_register(reg, text, linewise, false, host);
                if linewise {
                    self.change_lines_content(start, end, host);
                } else {
                    self.delete_range(start, end, false, host);
                }
                self.sm().mode = Mode::Insert;
            }
            Operator::Indent => self.indent_range(start, end, true, host),
            Operator::Outdent => self.indent_range(start, end, false, host),
            Operator::Lower => {
                self.transform_range(start, end, |c| c.to_lowercase().collect(), host)
            }
            Operator::Upper => {
                self.transform_range(start, end, |c| c.to_uppercase().collect(), host)
            }
            Operator::ToggleCase => self.transform_range(start, end, toggle_case, host),
        }
    }

    pub(super) fn delete_range(
        &mut self,
        start: usize,
        end: usize,
        linewise: bool,
        host: &mut dyn Host,
    ) {
        if start >= end {
            return;
        }
        // A linewise delete reaching end-of-buffer must also consume the preceding newline.
        let start = if linewise {
            Self::read(host, |d| {
                if end >= d.len_chars() && start > 0 && d.rope().char(start - 1) == '\n' {
                    start - 1
                } else {
                    start
                }
            })
            .unwrap_or(start)
        } else {
            start
        };
        Self::replace(host, start, end, String::new());
        if linewise {
            let target = Self::read(host, |d| {
                let at = d.clamp(start);
                let l = d.char_to_line(at);
                core_vim::first_non_blank(d, l)
            });
            if let Some(t) = target {
                Self::caret(host, t);
            }
        } else {
            Self::caret(host, start);
        }
    }

    fn change_lines_content(&mut self, start: usize, end: usize, host: &mut dyn Host) {
        let range = Self::read(host, |d| {
            let first = d.char_to_line(start);
            let last = d.char_to_line(end.saturating_sub(1).max(start));
            let content_start = d.line_to_char(first);
            let content_end = d.line_to_char(last) + d.line_len_chars(last);
            (content_start, content_end)
        });
        if let Some((cs, ce)) = range {
            Self::replace(host, cs, ce, String::new());
            Self::caret(host, cs);
        }
    }

    fn indent_range(&mut self, start: usize, end: usize, indent: bool, host: &mut dyn Host) {
        let Some(id) = host.active_doc() else {
            return;
        };
        // Build the per-line indent/outdent changes directly (mirrors `edit::indent`/`outdent`).
        let (changes, caret) = match host.workspace().documents.get(id) {
            Some(d) => {
                let fl = d.char_to_line(start);
                let ll = d.char_to_line(end.saturating_sub(1).max(start));
                let width = d.tab_width.max(1);
                let changes: Vec<Change> = (fl..=ll)
                    .filter_map(|l| line_indent_change(d, l, indent, width))
                    .collect();
                (changes, core_vim::first_non_blank(d, fl))
            }
            None => return,
        };
        if !changes.is_empty() {
            host.apply_transaction(id, Transaction::from_changes(changes));
        }
        // Re-resolve first-non-blank after the edit shifted columns.
        let caret = Self::read(host, |d| {
            core_vim::first_non_blank(d, d.char_to_line(d.clamp(caret)))
        })
        .unwrap_or(caret);
        Self::caret(host, caret);
    }

    pub(super) fn transform_range(
        &mut self,
        start: usize,
        end: usize,
        f: impl Fn(char) -> String,
        host: &mut dyn Host,
    ) {
        let out: String = Self::read(host, |d| {
            let (s, e) = (start.min(d.len_chars()), end.min(d.len_chars()));
            d.rope()
                .slice(s..e)
                .chars()
                .flat_map(|c| f(c).chars().collect::<Vec<_>>())
                .collect()
        })
        .unwrap_or_default();
        if out.is_empty() {
            return;
        }
        let end = Self::read(host, |d| end.min(d.len_chars())).unwrap_or(end);
        Self::replace(host, start, end, out);
        Self::caret(host, start);
    }

    pub(super) fn linewise_current(&mut self, op: Operator, host: &mut dyn Host) {
        let count = self.s().effective_count();
        let range = Self::read(host, |d| {
            let line = d.char_to_line(d.selections.primary().head);
            let last = (line + count - 1).min(d.len_lines().saturating_sub(1));
            let s = d.line_to_char(line);
            let e = if last + 1 < d.len_lines() {
                d.line_to_char(last + 1)
            } else {
                d.len_chars()
            };
            (s, e)
        });
        if let Some((s, e)) = range {
            self.apply_operator_range(op, s, e, true, host);
        }
    }

    pub(super) fn apply_text_object(&mut self, obj: TextObject, around: bool, host: &mut dyn Host) {
        let pos = Self::primary_head(host);
        let range = Self::read(host, |d| core_vim::text_object(d, pos, obj, around)).flatten();
        let Some((s, e)) = range else {
            self.sm().clear_pending();
            return;
        };
        let linewise = matches!(obj, TextObject::Paragraph);
        if matches!(self.s().mode, Mode::Visual | Mode::VisualLine) {
            Self::select(host, s, e.saturating_sub(1).max(s));
        } else if let Some(op) = self.s().operator {
            self.apply_operator_range(op, s, e, linewise, host);
        }
        self.sm().clear_pending();
    }
}
