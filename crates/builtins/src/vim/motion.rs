//! Motion resolution: motion keys to targets, `f`/`t` find-char, and applying the result.

use super::state::{FindPending, Mode, MotionKind};
use super::VimPlugin;
use editor_core::vim as core_vim;
use editor_core::vim::FindKind;
use editor_core::{motion, Document};
use editor_plugin::input::KeyCode;
use editor_plugin::Host;

/// Step `f` `count` times over `doc` from `start`, stopping if it stalls.
fn nth(doc: &Document, start: usize, count: usize, f: impl Fn(&Document, usize) -> usize) -> usize {
    let mut p = start;
    for _ in 0..count {
        let np = f(doc, p);
        if np == p {
            break;
        }
        p = np;
    }
    p
}

/// `%`: the partner of the bracket under the cursor, or of the next bracket on the line.
fn percent_target(doc: &Document) -> Option<usize> {
    let pos = doc.selections.primary().head;
    if let Some(m) = motion::matching_bracket(doc, pos) {
        return Some(m);
    }
    let line = doc.char_to_line(pos);
    let end = doc.line_to_char(line) + doc.line_len_chars(line);
    for i in pos..end {
        if matches!(doc.rope().char(i), '(' | ')' | '[' | ']' | '{' | '}') {
            return motion::matching_bracket(doc, i);
        }
    }
    None
}

impl VimPlugin {
    /// Resolve a motion key to `(target_offset, kind)` from the primary caret.
    pub(super) fn motion(
        &self,
        code: KeyCode,
        count_opt: Option<usize>,
        host: &dyn Host,
    ) -> Option<(usize, MotionKind)> {
        use MotionKind::*;
        let page = host.viewport_height().max(1);
        Self::read(host, |doc| {
            let pos = doc.selections.primary().head;
            let count = count_opt.unwrap_or(1);
            let line = doc.char_to_line(pos);
            let last_line = doc.len_lines().saturating_sub(1);
            let line_start = doc.line_to_char(line);
            let content_end = line_start + doc.line_len_chars(line);
            let result = match code {
                KeyCode::Char('h') | KeyCode::Left | KeyCode::Backspace => {
                    (pos.saturating_sub(count).max(line_start), Exclusive)
                }
                KeyCode::Char('l') | KeyCode::Right | KeyCode::Char(' ') => {
                    ((pos + count).min(content_end), Exclusive)
                }
                KeyCode::Char('j') | KeyCode::Down => {
                    (doc.line_to_char((line + count).min(last_line)), Linewise)
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    (doc.line_to_char(line.saturating_sub(count)), Linewise)
                }
                KeyCode::Char('w') => (
                    nth(doc, pos, count, |d, p| {
                        core_vim::next_word_start(d, p, false)
                    }),
                    Exclusive,
                ),
                KeyCode::Char('W') => (
                    nth(doc, pos, count, |d, p| {
                        core_vim::next_word_start(d, p, true)
                    }),
                    Exclusive,
                ),
                KeyCode::Char('b') => (
                    nth(doc, pos, count, |d, p| {
                        core_vim::prev_word_start(d, p, false)
                    }),
                    Exclusive,
                ),
                KeyCode::Char('B') => (
                    nth(doc, pos, count, |d, p| {
                        core_vim::prev_word_start(d, p, true)
                    }),
                    Exclusive,
                ),
                KeyCode::Char('e') => (
                    nth(doc, pos, count, |d, p| core_vim::next_word_end(d, p, false)),
                    Inclusive,
                ),
                KeyCode::Char('E') => (
                    nth(doc, pos, count, |d, p| core_vim::next_word_end(d, p, true)),
                    Inclusive,
                ),
                KeyCode::Char('0') | KeyCode::Home => (line_start, Exclusive),
                KeyCode::Char('^') => (core_vim::first_non_blank(doc, line), Exclusive),
                KeyCode::Char('$') | KeyCode::End => {
                    let l = (line + count - 1).min(last_line);
                    let ls_l = doc.line_to_char(l);
                    let ce = ls_l + doc.line_len_chars(l);
                    (ce.saturating_sub(1).max(ls_l), Inclusive)
                }
                KeyCode::Char('|') => {
                    let col = count.saturating_sub(1).min(doc.line_len_chars(line));
                    (line_start + col, Exclusive)
                }
                KeyCode::Char('{') => (
                    nth(doc, pos, count, core_vim::paragraph_backward),
                    Exclusive,
                ),
                KeyCode::Char('}') => {
                    (nth(doc, pos, count, core_vim::paragraph_forward), Exclusive)
                }
                KeyCode::Char('%') => (percent_target(doc)?, Inclusive),
                KeyCode::Char('G') => {
                    let l = match count_opt {
                        Some(c) => c.saturating_sub(1).min(last_line),
                        None => last_line,
                    };
                    (core_vim::first_non_blank(doc, l), Linewise)
                }
                KeyCode::Char('H') => {
                    let l = (doc.view.scroll_line + count.saturating_sub(1)).min(last_line);
                    (core_vim::first_non_blank(doc, l), Linewise)
                }
                KeyCode::Char('M') => {
                    let l = (doc.view.scroll_line + page / 2).min(last_line);
                    (core_vim::first_non_blank(doc, l), Linewise)
                }
                KeyCode::Char('L') => {
                    let bottom = (doc.view.scroll_line + page.saturating_sub(1)).min(last_line);
                    (
                        core_vim::first_non_blank(
                            doc,
                            bottom.saturating_sub(count.saturating_sub(1)),
                        ),
                        Linewise,
                    )
                }
                _ => return None,
            };
            Some(result)
        })
        .flatten()
    }

    pub(super) fn iter_motion(
        &self,
        step: impl Fn(&Document, usize) -> usize,
        count: usize,
        host: &dyn Host,
    ) -> usize {
        Self::read(host, |doc| {
            let mut p = doc.selections.primary().head;
            for _ in 0..count {
                let np = step(doc, p);
                if np == p {
                    break;
                }
                p = np;
            }
            p
        })
        .unwrap_or(0)
    }

    pub(super) fn motion_result(&mut self, target: usize, kind: MotionKind, host: &mut dyn Host) {
        match self.s().mode {
            Mode::Visual | Mode::VisualLine => {
                self.visual_set_head(target, host);
                self.sm().clear_pending();
            }
            _ => {
                if let Some(op) = self.s().operator {
                    let from = Self::primary_head(host);
                    self.apply_operator_kind(op, from, target, kind, host);
                } else {
                    self.caret_move(target, host);
                }
                self.sm().clear_pending();
            }
        }
    }

    pub(super) fn caret_move(&mut self, target: usize, host: &mut dyn Host) {
        Self::caret(host, target);
    }

    pub(super) fn move_lines(&mut self, delta: isize, extend: bool, host: &mut dyn Host) {
        if let Some(id) = host.active_doc() {
            host.move_lines(id, delta, extend);
        }
    }

    pub(super) fn find_apply(
        &mut self,
        fp: FindPending,
        ch: char,
        remember: bool,
        host: &mut dyn Host,
    ) {
        if remember {
            self.sm().last_find = Some((fp, ch));
        }
        let (kind, corekind) = match fp {
            FindPending::Find => (MotionKind::Inclusive, FindKind::Find),
            FindPending::Till => (MotionKind::Inclusive, FindKind::Till),
            FindPending::FindBack => (MotionKind::Exclusive, FindKind::FindBack),
            FindPending::TillBack => (MotionKind::Exclusive, FindKind::TillBack),
        };
        let target = Self::read(host, |d| {
            core_vim::find_char(d, d.selections.primary().head, ch, corekind)
        })
        .flatten();
        match target {
            Some(t) => self.motion_result(t, kind, host),
            None => self.sm().clear_pending(),
        }
    }

    pub(super) fn repeat_find(&mut self, reverse: bool, host: &mut dyn Host) {
        let Some((fp, ch)) = self.s().last_find else {
            return;
        };
        let fp = if reverse {
            match fp {
                FindPending::Find => FindPending::FindBack,
                FindPending::FindBack => FindPending::Find,
                FindPending::Till => FindPending::TillBack,
                FindPending::TillBack => FindPending::Till,
            }
        } else {
            fp
        };
        self.find_apply(fp, ch, false, host);
    }
}
