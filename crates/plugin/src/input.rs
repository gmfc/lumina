//! A crossterm-free key representation handed to plugins that intercept raw input.
//!
//! The kernel must not depend on `crossterm` (that's an `lumina` concern), so raw-key
//! interception — [`crate::Plugin::capture_key`] — speaks in these primitive types. The app
//! translates `crossterm::event::KeyEvent` into a [`Key`] at the boundary. This mirrors the
//! subset of key codes the editor actually binds (see `lumina`'s `keymap.rs`), which is
//! all a modal layer (vim) or a focused terminal needs to pre-empt chord resolution.

/// A key code: the crossterm-free twin of the subset of `crossterm::event::KeyCode` the editor
/// binds. `Char` already carries the shift-folded character (the app lowercases/uppercases at
/// the boundary exactly as the chord keymap does), so `shift` on [`Key`] is meaningful only for
/// the named keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyCode {
    Char(char),
    Enter,
    Tab,
    BackTab,
    Esc,
    Backspace,
    Delete,
    Insert,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    /// A function key, `F(1)`..=`F(12)`.
    F(u8),
}

/// A normalized key press: a [`KeyCode`] plus modifier flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Key {
    pub code: KeyCode,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl Key {
    /// A key with no modifiers.
    pub fn new(code: KeyCode) -> Key {
        Key {
            code,
            ctrl: false,
            alt: false,
            shift: false,
        }
    }

    /// A bare character key (no modifiers).
    pub fn char(c: char) -> Key {
        Key::new(KeyCode::Char(c))
    }

    /// Builder: set the ctrl modifier.
    pub fn with_ctrl(mut self) -> Key {
        self.ctrl = true;
        self
    }

    /// Builder: set the alt modifier.
    pub fn with_alt(mut self) -> Key {
        self.alt = true;
        self
    }

    /// Builder: set the shift modifier.
    pub fn with_shift(mut self) -> Key {
        self.shift = true;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builders_set_modifiers() {
        let k = Key::char('d').with_ctrl();
        assert_eq!(k.code, KeyCode::Char('d'));
        assert!(k.ctrl && !k.alt && !k.shift);
        let k = Key::new(KeyCode::Enter).with_alt().with_shift();
        assert!(k.alt && k.shift && !k.ctrl);
    }
}
