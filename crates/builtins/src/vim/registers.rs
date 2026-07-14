//! Register storage and lookup: named/uppercase-append registers, unnamed, yank, clipboard.

use super::state::Register;
use super::VimPlugin;
use editor_plugin::Host;

impl VimPlugin {
    pub(super) fn store_register(
        &mut self,
        reg: Option<char>,
        text: String,
        linewise: bool,
        is_yank: bool,
        host: &mut dyn Host,
    ) {
        match reg {
            Some('_') => {}
            Some('+') | Some('*') => {
                host.clipboard_write(text.clone());
                self.sm().unnamed = Register { text, linewise };
            }
            Some(c) if c.is_ascii_alphabetic() => {
                let lower = c.to_ascii_lowercase();
                if c.is_ascii_uppercase() {
                    let entry = self.sm().registers.entry(lower).or_default();
                    entry.text.push_str(&text);
                    entry.linewise = linewise;
                    let combined = self.s().registers[&lower].clone();
                    self.sm().unnamed = combined;
                } else {
                    self.sm().registers.insert(
                        lower,
                        Register {
                            text: text.clone(),
                            linewise,
                        },
                    );
                    self.sm().unnamed = Register { text, linewise };
                }
            }
            _ => {
                self.sm().unnamed = Register {
                    text: text.clone(),
                    linewise,
                };
                if is_yank {
                    self.sm().yanked = Register { text, linewise };
                }
            }
        }
    }

    pub(super) fn read_register(&mut self, reg: Option<char>, host: &mut dyn Host) -> Register {
        match reg {
            Some('+') | Some('*') => Register {
                text: host.clipboard_read(),
                linewise: false,
            },
            Some('0') => self.s().yanked.clone(),
            Some('_') => Register::default(),
            Some(c) if c.is_ascii_alphabetic() => self
                .s()
                .registers
                .get(&c.to_ascii_lowercase())
                .cloned()
                .unwrap_or_default(),
            _ => self.s().unnamed.clone(),
        }
    }
}
