//! The `/` and `?` search line, `n`/`N` repeat, and `*`/`#` search-word.

use super::VimPlugin;
use editor_core::motion;
use editor_plugin::input::{Key, KeyCode};
use editor_plugin::Host;

/// Naive literal substring search over `chars` for `pat`, wrapping around. Returns the match start.
fn search_literal(chars: &[char], pat: &[char], start: usize, forward: bool) -> Option<usize> {
    if pat.is_empty() || chars.len() < pat.len() {
        return None;
    }
    let last_start = chars.len() - pat.len();
    let matches_at = |i: usize| chars[i..i + pat.len()] == *pat;
    if forward {
        let begin = start.min(chars.len());
        (begin..=last_start)
            .find(|&i| matches_at(i))
            .or_else(|| (0..begin.min(last_start + 1)).find(|&i| matches_at(i)))
    } else {
        let begin = start.min(last_start);
        (0..=begin)
            .rev()
            .find(|&i| matches_at(i))
            .or_else(|| (begin + 1..=last_start).rev().find(|&i| matches_at(i)))
    }
}

impl VimPlugin {
    pub(super) fn open_search(&mut self, forward: bool) {
        self.sm().search = Some((forward, String::new()));
        self.sm().recording = None;
    }

    pub(super) fn search_key(&mut self, key: Key, host: &mut dyn Host) {
        match key.code {
            KeyCode::Esc => self.sm().search = None,
            KeyCode::Enter => {
                let (fwd, pat) = self.sm().search.take().unwrap_or((true, String::new()));
                if !pat.is_empty() {
                    self.sm().last_search = Some((fwd, pat.clone()));
                    self.do_search(fwd, &pat, true, host);
                }
            }
            KeyCode::Backspace => {
                let empty = {
                    let (_, buf) = self.sm().search.as_mut().unwrap();
                    buf.pop();
                    buf.is_empty()
                };
                if empty {
                    self.sm().search = None;
                }
            }
            KeyCode::Char(c) if !key.ctrl => {
                if let Some((_, buf)) = self.sm().search.as_mut() {
                    buf.push(c);
                }
            }
            _ => {}
        }
    }

    fn do_search(&mut self, forward: bool, pat: &str, from_next: bool, host: &mut dyn Host) {
        let found = Self::read(host, |d| {
            let chars: Vec<char> = d.rope().chars().collect();
            let pat: Vec<char> = pat.chars().collect();
            let head = d.selections.primary().head;
            let start = if from_next {
                if forward {
                    head + 1
                } else {
                    head.saturating_sub(1)
                }
            } else {
                head
            };
            search_literal(&chars, &pat, start, forward)
        })
        .flatten();
        if let Some(pos) = found {
            Self::caret(host, pos);
        } else {
            host.notify(format!("Pattern not found: {pat}"));
        }
    }

    pub(super) fn search_next(&mut self, reverse: bool, host: &mut dyn Host) {
        let Some((fwd, pat)) = self.s().last_search.clone() else {
            return;
        };
        let dir = if reverse { !fwd } else { fwd };
        self.do_search(dir, &pat, true, host);
    }

    pub(super) fn search_word(&mut self, forward: bool, host: &mut dyn Host) {
        let word = Self::read(host, |d| {
            let head = d.selections.primary().head;
            let (s, e) = motion::word_at(d, head);
            d.rope().slice(s..e).to_string()
        });
        if let Some(word) = word {
            if !word.trim().is_empty() {
                self.sm().last_search = Some((forward, word.clone()));
                self.do_search(forward, &word, true, host);
            }
        }
    }
}
