//! Chord-trie keymap: sequences of key chords → command ids (plan §5). Supports multi-key
//! chords (`ctrl+k ctrl+s`), is built from defaults + config overrides, and reports partial
//! matches so the caller can arm a "pending chord" state.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// A single normalized key chord.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chord {
    pub code: KeyCode,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl Chord {
    /// Normalize a crossterm key event into a chord. Character shift is folded into the
    /// char itself (crossterm already uppercases), so we only track shift for named keys.
    pub fn from_event(key: KeyEvent) -> Chord {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let mut shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let code = match key.code {
            KeyCode::Char(c) => {
                shift = false;
                KeyCode::Char(c.to_ascii_lowercase())
            }
            other => other,
        };
        Chord {
            code,
            ctrl,
            alt,
            shift,
        }
    }

    /// Parse a chord like `"ctrl+shift+p"` or `"enter"`.
    pub fn parse(s: &str) -> Option<Chord> {
        let mut ctrl = false;
        let mut alt = false;
        let mut shift = false;
        let mut code = None;
        for part in s.split('+') {
            match part.trim().to_ascii_lowercase().as_str() {
                "ctrl" | "control" => ctrl = true,
                "alt" | "option" => alt = true,
                "shift" => shift = true,
                other => code = Some(parse_code(other)?),
            }
        }
        let code = code?;
        // For a single character binding, fold shift into the char.
        let shift = if matches!(code, KeyCode::Char(_)) {
            false
        } else {
            shift
        };
        Some(Chord {
            code,
            ctrl,
            alt,
            shift,
        })
    }
}

/// A display label for one chord (`Ctrl+Shift+P`, `Ctrl+\`, `F12`, `Enter`, `Ctrl+``…).
fn chord_label(c: &Chord) -> String {
    let mut s = String::new();
    if c.ctrl {
        s.push_str("Ctrl+");
    }
    if c.alt {
        s.push_str("Alt+");
    }
    if c.shift {
        s.push_str("Shift+");
    }
    let key = match c.code {
        KeyCode::Char(' ') => "Space".to_string(),
        KeyCode::Char(ch) => ch.to_ascii_uppercase().to_string(),
        KeyCode::Enter => "Enter".to_string(),
        KeyCode::Tab => "Tab".to_string(),
        KeyCode::BackTab => "Shift+Tab".to_string(),
        KeyCode::Esc => "Esc".to_string(),
        KeyCode::Up => "Up".to_string(),
        KeyCode::Down => "Down".to_string(),
        KeyCode::Left => "Left".to_string(),
        KeyCode::Right => "Right".to_string(),
        KeyCode::Home => "Home".to_string(),
        KeyCode::End => "End".to_string(),
        KeyCode::PageUp => "PageUp".to_string(),
        KeyCode::PageDown => "PageDown".to_string(),
        KeyCode::Backspace => "Backspace".to_string(),
        KeyCode::Delete => "Delete".to_string(),
        KeyCode::Insert => "Insert".to_string(),
        KeyCode::F(n) => format!("F{n}"),
        other => format!("{other:?}"),
    };
    s.push_str(&key);
    s
}

fn parse_code(s: &str) -> Option<KeyCode> {
    let code = match s {
        "enter" | "return" => KeyCode::Enter,
        "tab" => KeyCode::Tab,
        "backtab" => KeyCode::BackTab,
        "esc" | "escape" => KeyCode::Esc,
        "space" => KeyCode::Char(' '),
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" => KeyCode::PageUp,
        "pagedown" => KeyCode::PageDown,
        "backspace" => KeyCode::Backspace,
        "delete" | "del" => KeyCode::Delete,
        "insert" => KeyCode::Insert,
        f if f.starts_with('f') && f[1..].parse::<u8>().is_ok() => KeyCode::F(f[1..].parse().ok()?),
        c if c.chars().count() == 1 => KeyCode::Char(c.chars().next()?),
        _ => return None,
    };
    Some(code)
}

/// Result of feeding a chord sequence to the keymap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolve {
    /// No binding and no partial match.
    None,
    /// A prefix of one or more bindings; arm the pending state and wait.
    Pending,
    /// A complete binding.
    Command(String),
}

/// Chord-sequence → command-id bindings.
pub struct Keymap {
    bindings: Vec<(Vec<Chord>, String)>,
}

impl Keymap {
    pub fn new() -> Keymap {
        Keymap {
            bindings: Vec::new(),
        }
    }

    /// Build from `(chord-string, id)` pairs (defaults, then config overrides).
    pub fn from_pairs<'a, I: IntoIterator<Item = (&'a str, &'a str)>>(pairs: I) -> Keymap {
        let mut km = Keymap::new();
        for (chords, id) in pairs {
            km.bind(chords, id);
        }
        km
    }

    /// Bind a (possibly multi-chord) sequence to an id. A later bind for the same sequence
    /// overrides an earlier one (so config wins over defaults).
    pub fn bind(&mut self, chord_seq: &str, id: &str) {
        let seq: Vec<Chord> = chord_seq
            .split_whitespace()
            .filter_map(Chord::parse)
            .collect();
        if seq.is_empty() {
            return;
        }
        if let Some(existing) = self.bindings.iter_mut().find(|(s, _)| *s == seq) {
            existing.1 = id.to_string();
        } else {
            self.bindings.push((seq, id.to_string()));
        }
    }

    /// The first bound chord sequence for `id`, formatted for display (e.g. `"Ctrl+K Ctrl+S"`),
    /// so UI hints track config remaps rather than hard-coded defaults. `None` when nothing is
    /// bound to `id`.
    pub fn binding_label(&self, id: &str) -> Option<String> {
        self.bindings
            .iter()
            .find(|(_, bound)| bound == id)
            .map(|(seq, _)| seq.iter().map(chord_label).collect::<Vec<_>>().join(" "))
    }

    /// Resolve a pending chord sequence.
    pub fn resolve(&self, seq: &[Chord]) -> Resolve {
        let mut partial = false;
        for (chords, id) in &self.bindings {
            if chords.as_slice() == seq {
                return Resolve::Command(id.clone());
            }
            if chords.len() > seq.len() && &chords[..seq.len()] == seq {
                partial = true;
            }
        }
        if partial {
            Resolve::Pending
        } else {
            Resolve::None
        }
    }
}

impl Default for Keymap {
    fn default() -> Self {
        Keymap::from_pairs(crate::commands::default_bindings().iter().copied())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(code: KeyCode, m: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, m)
    }

    #[test]
    fn parses_and_matches_simple_chord() {
        let km = Keymap::from_pairs([("ctrl+s", "file.save")]);
        let c = Chord::from_event(ev(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert_eq!(km.resolve(&[c]), Resolve::Command("file.save".into()));
    }

    #[test]
    fn reports_pending_then_full_for_multichord() {
        let km = Keymap::from_pairs([("ctrl+k ctrl+s", "keys.show")]);
        let k1 = Chord::from_event(ev(KeyCode::Char('k'), KeyModifiers::CONTROL));
        let k2 = Chord::from_event(ev(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert_eq!(km.resolve(std::slice::from_ref(&k1)), Resolve::Pending);
        assert_eq!(km.resolve(&[k1, k2]), Resolve::Command("keys.show".into()));
    }

    #[test]
    fn config_overrides_default() {
        let mut km = Keymap::from_pairs([("ctrl+s", "file.save")]);
        km.bind("ctrl+s", "file.saveAs");
        let c = Chord::from_event(ev(KeyCode::Char('s'), KeyModifiers::CONTROL));
        assert_eq!(km.resolve(&[c]), Resolve::Command("file.saveAs".into()));
    }

    #[test]
    fn binding_label_reflects_overrides_and_formats() {
        let mut km = Keymap::from_pairs([
            ("ctrl+g", "view.gotoLine"),
            ("ctrl+k ctrl+s", "file.saveAs"),
            ("f12", "lsp.gotoDefinition"),
            ("ctrl+\\", "cursor.jumpToBracket"),
        ]);
        assert_eq!(km.binding_label("view.gotoLine").as_deref(), Some("Ctrl+G"));
        assert_eq!(
            km.binding_label("file.saveAs").as_deref(),
            Some("Ctrl+K Ctrl+S")
        );
        assert_eq!(
            km.binding_label("lsp.gotoDefinition").as_deref(),
            Some("F12")
        );
        assert_eq!(
            km.binding_label("cursor.jumpToBracket").as_deref(),
            Some("Ctrl+\\")
        );
        assert_eq!(km.binding_label("nope"), None);
        // A config override that repoints a chord moves the label with it.
        km.bind("ctrl+g", "app.quit");
        assert_eq!(km.binding_label("app.quit").as_deref(), Some("Ctrl+G"));
        assert_eq!(km.binding_label("view.gotoLine"), None);
    }

    #[test]
    fn unbound_is_none() {
        let km = Keymap::from_pairs([("ctrl+s", "file.save")]);
        let c = Chord::from_event(ev(KeyCode::Char('j'), KeyModifiers::NONE));
        assert_eq!(km.resolve(&[c]), Resolve::None);
    }
}
