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
    /// Normalize a crossterm key event into a chord.
    ///
    /// For a bare character, shift *is* the character (`p` vs `P`) and crossterm has already
    /// applied it, so folding it away is correct — otherwise every capital letter would need its
    /// own binding. Once Ctrl or Alt is held the keystroke is a chord rather than text, and shift
    /// is a real modifier: `ctrl+shift+p` must stay distinct from `ctrl+p`, or one of them
    /// silently overwrites the other in the keymap (see [`Keymap::bind`]).
    ///
    /// Terminals without the kitty keyboard protocol cannot *report* that distinction — they send
    /// the same bytes for both — so shift simply arrives unset there and
    /// [`Keymap::resolve`] falls back to the shifted binding when the unshifted one is free.
    pub fn from_event(key: KeyEvent) -> Chord {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        let mut shift = key.modifiers.contains(KeyModifiers::SHIFT);
        let code = match key.code {
            KeyCode::Char(c) => {
                shift &= ctrl || alt;
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
        // Mirror `from_event`: shift is the character itself for plain typing, and a real
        // modifier only alongside Ctrl/Alt. Keeping the two in step is what makes
        // `ctrl+shift+p` a different binding from `ctrl+p` instead of a silent overwrite.
        let shift = if matches!(code, KeyCode::Char(_)) {
            shift && (ctrl || alt)
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
/// Shared with the pending-chord hint so an armed prefix is spelled the same way the reference
/// and the palette spell it.
pub fn chord_label(c: &Chord) -> String {
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
    conflicts: Vec<Conflict>,
}

/// A binding that was overwritten by a later one for the same chord sequence. Later tiers are
/// *meant* to win (a user remap beats a plugin beats a default), so this is a record rather than
/// an error — but a default clobbering another default means a command shipped with no way to
/// reach it, which is what [`Keymap::conflicts`] exists to catch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Conflict {
    /// The contested chord sequence, formatted the way the reference spells it.
    pub chord: String,
    /// The id that lost the chord.
    pub replaced: String,
    /// The id that now owns it.
    pub winner: String,
}

impl Keymap {
    pub fn new() -> Keymap {
        Keymap {
            bindings: Vec::new(),
            conflicts: Vec::new(),
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
    /// overrides an earlier one (so config wins over plugin contributions wins over defaults),
    /// and the displaced id is recorded in [`Keymap::conflicts`] so an accidental clobber inside
    /// one tier is visible instead of silent.
    ///
    /// A sequence whose chords don't parse binds nothing — a typo in `[keys]` leaves the previous
    /// binding alone rather than stealing the chord.
    pub fn bind(&mut self, chord_seq: &str, id: &str) {
        let parts: Vec<&str> = chord_seq.split_whitespace().collect();
        let seq: Vec<Chord> = parts.iter().filter_map(|c| Chord::parse(c)).collect();
        // `filter_map` would otherwise silently shorten `ctrl+k ctrl+shft+s` to `ctrl+k`, binding
        // a *prefix* of what was asked for and swallowing every chord under it.
        if seq.is_empty() || seq.len() != parts.len() {
            return;
        }
        if let Some(existing) = self.bindings.iter_mut().find(|(s, _)| *s == seq) {
            if existing.1 != id {
                self.conflicts.push(Conflict {
                    chord: seq.iter().map(chord_label).collect::<Vec<_>>().join(" "),
                    replaced: std::mem::replace(&mut existing.1, id.to_string()),
                    winner: id.to_string(),
                });
            }
        } else {
            self.bindings.push((seq, id.to_string()));
        }
    }

    /// Bindings that were overwritten, oldest first. See [`Conflict`].
    pub fn conflicts(&self) -> &[Conflict] {
        &self.conflicts
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

    /// Every binding as `(display label, command id)`, in bind order (defaults, then plugin
    /// contributions, then user overrides). The source for the in-app keybinding reference, which
    /// therefore can't drift from what the keys actually do.
    pub fn entries(&self) -> impl Iterator<Item = (String, &str)> {
        self.bindings.iter().map(|(seq, id)| {
            (
                seq.iter().map(chord_label).collect::<Vec<_>>().join(" "),
                id.as_str(),
            )
        })
    }

    /// The continuations available after the armed prefix `seq`: `(next chord's label, command
    /// id)` for every binding that starts with it. Empty when nothing extends `seq`.
    pub fn continuations(&self, seq: &[Chord]) -> Vec<(String, &str)> {
        self.bindings
            .iter()
            .filter(|(chords, _)| chords.len() > seq.len() && chords[..seq.len()] == *seq)
            .map(|(chords, id)| (chord_label(&chords[seq.len()]), id.as_str()))
            .collect()
    }

    /// Resolve a pending chord sequence.
    ///
    /// Tried exactly first, then — for terminals that cannot report Shift on a Ctrl/Alt chord,
    /// which is most of them without the kitty keyboard protocol — again with Shift set. So
    /// `Ctrl+Shift+O` arrives as `Ctrl+O` on a plain xterm and still reaches Document Symbols,
    /// provided nothing claims `Ctrl+O` outright. Where both are bound the exact match wins and
    /// the shifted binding is simply unreachable there, which is the honest outcome: the terminal
    /// genuinely sends the same bytes for both.
    pub fn resolve(&self, seq: &[Chord]) -> Resolve {
        let exact = self.resolve_exact(seq);
        if let Resolve::Command(_) = exact {
            return exact;
        }
        let shifted = shift_variant(seq);
        if shifted.as_slice() != seq {
            match (self.resolve_exact(&shifted), &exact) {
                (cmd @ Resolve::Command(_), _) => return cmd,
                (Resolve::Pending, Resolve::None) => return Resolve::Pending,
                _ => {}
            }
        }
        exact
    }

    fn resolve_exact(&self, seq: &[Chord]) -> Resolve {
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

/// The same sequence with Shift set on every Ctrl/Alt character chord — the binding a terminal
/// without the kitty keyboard protocol *would* have sent if it could express it.
fn shift_variant(seq: &[Chord]) -> Vec<Chord> {
    seq.iter()
        .map(|c| {
            let shiftable = matches!(c.code, KeyCode::Char(_)) && (c.ctrl || c.alt) && !c.shift;
            Chord {
                shift: c.shift || shiftable,
                ..c.clone()
            }
        })
        .collect()
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
