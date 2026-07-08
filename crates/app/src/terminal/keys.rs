//! Key/color translation between the editor's input model and the byte/attribute conventions
//! a PTY expects, plus the default-shell resolver.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::Color;

/// The default shell for a new terminal: the config override, else the platform's usual shell.
pub fn default_shell(config_override: Option<&str>) -> String {
    if let Some(s) = config_override {
        let s = s.trim();
        if !s.is_empty() {
            return s.to_string();
        }
    }
    #[cfg(windows)]
    {
        std::env::var("ComSpec").unwrap_or_else(|_| "cmd.exe".to_string())
    }
    #[cfg(not(windows))]
    {
        std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string())
    }
}

/// Map a `vt100` cell color to a ratatui color. `None` means "terminal default" — the renderer
/// leaves that channel unset (`Reset`) so the surrounding theme shows through.
pub fn vt_color(c: vt100::Color) -> Option<Color> {
    match c {
        vt100::Color::Default => None,
        vt100::Color::Idx(i) => Some(Color::Indexed(i)),
        vt100::Color::Rgb(r, g, b) => Some(Color::Rgb(r, g, b)),
    }
}

/// Translate a key event into the byte sequence a terminal expects, or `None` when the key has
/// no terminal encoding. `app_cursor` selects the application-cursor-key form for arrows / home
/// / end (shells and full-screen apps toggle this via DECCKM).
pub fn key_to_bytes(key: &KeyEvent, app_cursor: bool) -> Option<Vec<u8>> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let alt = key.modifiers.contains(KeyModifiers::ALT);

    let mut out: Vec<u8> = Vec::new();
    match key.code {
        KeyCode::Char(c) => {
            if ctrl {
                match ctrl_byte(c) {
                    Some(b) => out.push(b),
                    None => push_char(&mut out, c),
                }
            } else {
                push_char(&mut out, c);
            }
            // Alt/Meta prefixes a printable with ESC (readline word motions, etc.).
            if alt {
                out.insert(0, 0x1b);
            }
        }
        KeyCode::Enter => out.push(b'\r'),
        KeyCode::Tab => out.push(b'\t'),
        KeyCode::BackTab => out.extend_from_slice(b"\x1b[Z"),
        KeyCode::Backspace => out.push(0x7f),
        KeyCode::Esc => out.push(0x1b),
        KeyCode::Left => out = csi_arrow(b'D', app_cursor),
        KeyCode::Right => out = csi_arrow(b'C', app_cursor),
        KeyCode::Up => out = csi_arrow(b'A', app_cursor),
        KeyCode::Down => out = csi_arrow(b'B', app_cursor),
        KeyCode::Home => out = csi_arrow(b'H', app_cursor),
        KeyCode::End => out = csi_arrow(b'F', app_cursor),
        KeyCode::PageUp => out.extend_from_slice(b"\x1b[5~"),
        KeyCode::PageDown => out.extend_from_slice(b"\x1b[6~"),
        KeyCode::Delete => out.extend_from_slice(b"\x1b[3~"),
        KeyCode::Insert => out.extend_from_slice(b"\x1b[2~"),
        KeyCode::F(n) => out = function_key(n)?,
        _ => return None,
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

/// Append `c` as UTF-8.
fn push_char(out: &mut Vec<u8>, c: char) {
    let mut buf = [0u8; 4];
    out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
}

/// The control byte for `Ctrl+<c>`, if one exists (C0 range).
fn ctrl_byte(c: char) -> Option<u8> {
    match c.to_ascii_lowercase() {
        'a'..='z' => Some(c.to_ascii_lowercase() as u8 - b'a' + 1),
        ' ' | '@' => Some(0),
        '[' => Some(0x1b),
        '\\' => Some(0x1c),
        ']' => Some(0x1d),
        '^' => Some(0x1e),
        '_' => Some(0x1f),
        _ => None,
    }
}

/// A cursor / navigation escape ending in `final_byte`, in normal (`ESC [`) or application
/// (`ESC O`) form.
fn csi_arrow(final_byte: u8, app_cursor: bool) -> Vec<u8> {
    let intro = if app_cursor { b'O' } else { b'[' };
    vec![0x1b, intro, final_byte]
}

/// The xterm escape for function key `n` (F1–F12), or `None` beyond that.
fn function_key(n: u8) -> Option<Vec<u8>> {
    let seq: &[u8] = match n {
        1 => b"\x1bOP",
        2 => b"\x1bOQ",
        3 => b"\x1bOR",
        4 => b"\x1bOS",
        5 => b"\x1b[15~",
        6 => b"\x1b[17~",
        7 => b"\x1b[18~",
        8 => b"\x1b[19~",
        9 => b"\x1b[20~",
        10 => b"\x1b[21~",
        11 => b"\x1b[23~",
        12 => b"\x1b[24~",
        _ => return None,
    };
    Some(seq.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyEvent;

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn plain_and_control_chars_encode() {
        assert_eq!(
            key_to_bytes(&key(KeyCode::Char('a'), KeyModifiers::NONE), false),
            Some(vec![b'a'])
        );
        // Ctrl+C → ETX (0x03), the SIGINT byte.
        assert_eq!(
            key_to_bytes(&key(KeyCode::Char('c'), KeyModifiers::CONTROL), false),
            Some(vec![0x03])
        );
        // Ctrl+A → SOH (0x01).
        assert_eq!(
            key_to_bytes(&key(KeyCode::Char('a'), KeyModifiers::CONTROL), false),
            Some(vec![0x01])
        );
        // Alt+b → ESC b (readline "word back").
        assert_eq!(
            key_to_bytes(&key(KeyCode::Char('b'), KeyModifiers::ALT), false),
            Some(vec![0x1b, b'b'])
        );
    }

    #[test]
    fn special_keys_encode() {
        assert_eq!(
            key_to_bytes(&key(KeyCode::Enter, KeyModifiers::NONE), false),
            Some(vec![b'\r'])
        );
        assert_eq!(
            key_to_bytes(&key(KeyCode::Backspace, KeyModifiers::NONE), false),
            Some(vec![0x7f])
        );
        assert_eq!(
            key_to_bytes(&key(KeyCode::Tab, KeyModifiers::NONE), false),
            Some(vec![b'\t'])
        );
        assert_eq!(
            key_to_bytes(&key(KeyCode::Delete, KeyModifiers::NONE), false),
            Some(b"\x1b[3~".to_vec())
        );
        assert_eq!(
            key_to_bytes(&key(KeyCode::F(1), KeyModifiers::NONE), false),
            Some(b"\x1bOP".to_vec())
        );
        assert_eq!(
            key_to_bytes(&key(KeyCode::F(5), KeyModifiers::NONE), false),
            Some(b"\x1b[15~".to_vec())
        );
    }

    #[test]
    fn arrows_respect_application_cursor_mode() {
        assert_eq!(
            key_to_bytes(&key(KeyCode::Up, KeyModifiers::NONE), false),
            Some(b"\x1b[A".to_vec())
        );
        assert_eq!(
            key_to_bytes(&key(KeyCode::Up, KeyModifiers::NONE), true),
            Some(b"\x1bOA".to_vec())
        );
        assert_eq!(
            key_to_bytes(&key(KeyCode::Home, KeyModifiers::NONE), false),
            Some(b"\x1b[H".to_vec())
        );
    }

    #[test]
    fn color_mapping() {
        assert_eq!(vt_color(vt100::Color::Default), None);
        assert_eq!(vt_color(vt100::Color::Idx(4)), Some(Color::Indexed(4)));
        assert_eq!(
            vt_color(vt100::Color::Rgb(10, 20, 30)),
            Some(Color::Rgb(10, 20, 30))
        );
    }

    #[test]
    fn default_shell_prefers_override() {
        assert_eq!(default_shell(Some("/bin/zsh")), "/bin/zsh");
        assert_eq!(default_shell(Some("  ")), default_shell(None));
        assert!(!default_shell(None).is_empty());
    }
}
