//! Clipboard: the system clipboard (`arboard`) with an OSC 52 fallback for SSH/headless
//! sessions and an in-process register that always works (plan §6, "Clipboard").
//!
//! Copy/cut write to all three; paste prefers the system clipboard and falls back to the
//! internal register, so yank/paste works whether or not a display server is reachable.

use std::io::{self, Write};

/// A three-tier clipboard. `system` is `None` when no display server is available.
pub struct Clipboard {
    system: Option<arboard::Clipboard>,
    register: String,
    /// Emit OSC 52 so the *outer* terminal (e.g. over SSH) captures copies too.
    osc52: bool,
}

impl Clipboard {
    pub fn new() -> Clipboard {
        // Remote sessions can't reach a local clipboard daemon; OSC 52 lets the controlling
        // terminal store the copy instead.
        let osc52 =
            std::env::var_os("SSH_CONNECTION").is_some() || std::env::var_os("SSH_TTY").is_some();
        Clipboard {
            system: arboard::Clipboard::new().ok(),
            register: String::new(),
            osc52,
        }
    }

    /// Copy `text` to every available sink.
    pub fn set(&mut self, text: String) {
        if let Some(cb) = &mut self.system {
            let _ = cb.set_text(&text);
        }
        if self.osc52 {
            let _ = emit_osc52(&text);
        }
        self.register = text;
    }

    /// Read the clipboard, preferring the system clipboard and falling back to the register.
    pub fn get(&mut self) -> String {
        if let Some(cb) = &mut self.system {
            if let Ok(text) = cb.get_text() {
                return text;
            }
        }
        self.register.clone()
    }
}

impl Default for Clipboard {
    fn default() -> Self {
        Clipboard::new()
    }
}

/// Write an OSC 52 clipboard-set sequence to stdout: `ESC ] 52 ; c ; <base64> BEL`.
fn emit_osc52(text: &str) -> io::Result<()> {
    let seq = format!("\x1b]52;c;{}\x07", base64_encode(text.as_bytes()));
    let mut out = io::stdout();
    out.write_all(seq.as_bytes())?;
    out.flush()
}

/// Minimal standard-alphabet base64 (dependency-free; OSC 52 payloads are small).
fn base64_encode(input: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(input.len().div_ceil(3) * 4);
    for chunk in input.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(ALPHABET[((n >> 18) & 0x3f) as usize] as char);
        out.push(ALPHABET[((n >> 12) & 0x3f) as usize] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[((n >> 6) & 0x3f) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[(n & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_matches_known_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foob"), "Zm9vYg==");
        assert_eq!(base64_encode(b"hello"), "aGVsbG8=");
    }

    #[test]
    fn register_round_trips_without_system_clipboard() {
        let mut cb = Clipboard {
            system: None,
            register: String::new(),
            osc52: false,
        };
        cb.set("copied".into());
        assert_eq!(cb.get(), "copied");
    }
}
