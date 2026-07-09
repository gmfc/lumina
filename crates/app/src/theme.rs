//! Theme: maps tree-sitter capture names to terminal styles. Truecolor by default,
//! downsampled to the 16-color palette when `COLORTERM` doesn't advertise truecolor
//! (plan §4 theming). Loadable from TOML; ships with a built-in dark default.

use std::collections::HashMap;

use ratatui::style::{Color, Modifier, Style};

/// A resolved style for a capture.
#[derive(Debug, Clone, Copy)]
pub struct CaptureStyle {
    pub fg: Color,
    pub modifier: Modifier,
}

pub struct Theme {
    map: HashMap<String, CaptureStyle>,
    pub selection_bg: Color,
    pub gutter_fg: Color,
    dark: bool,
}

/// Detect truecolor support from the environment (plan §4).
pub fn truecolor_supported() -> bool {
    std::env::var("COLORTERM")
        .map(|v| v.contains("truecolor") || v.contains("24bit"))
        .unwrap_or(false)
}

impl Theme {
    /// The built-in dark theme, choosing truecolor RGB or nearest ANSI per `truecolor`.
    pub fn default_dark(truecolor: bool) -> Theme {
        let c = |r: u8, g: u8, b: u8, ansi: Color| -> Color {
            if truecolor {
                Color::Rgb(r, g, b)
            } else {
                ansi
            }
        };
        let plain = |fg: Color| CaptureStyle {
            fg,
            modifier: Modifier::empty(),
        };
        let italic = |fg: Color| CaptureStyle {
            fg,
            modifier: Modifier::ITALIC,
        };

        let mut map = HashMap::new();
        let mut set = |k: &str, v: CaptureStyle| {
            map.insert(k.to_string(), v);
        };
        set("keyword", plain(c(198, 120, 221, Color::Magenta)));
        set("function", plain(c(97, 175, 239, Color::Blue)));
        set("function.macro", plain(c(86, 182, 194, Color::Cyan)));
        set("type", plain(c(229, 192, 123, Color::Yellow)));
        set("constructor", plain(c(229, 192, 123, Color::Yellow)));
        set("string", plain(c(152, 195, 121, Color::Green)));
        set("string.special", plain(c(86, 182, 194, Color::Cyan)));
        set("number", plain(c(209, 154, 102, Color::Yellow)));
        set("constant", plain(c(209, 154, 102, Color::Yellow)));
        set("constant.builtin", plain(c(86, 182, 194, Color::Cyan)));
        set("comment", italic(c(92, 99, 112, Color::DarkGray)));
        set("property", plain(c(224, 108, 117, Color::Red)));
        set("label", plain(c(224, 108, 117, Color::Red)));
        set("variable", plain(c(220, 223, 228, Color::White)));
        set("variable.builtin", plain(c(224, 108, 117, Color::Red)));
        set("parameter", plain(c(220, 223, 228, Color::White)));
        set("operator", plain(c(171, 178, 191, Color::Gray)));
        set("punctuation", plain(c(171, 178, 191, Color::Gray)));
        set("punctuation.bracket", plain(c(171, 178, 191, Color::Gray)));
        set(
            "punctuation.delimiter",
            plain(c(171, 178, 191, Color::Gray)),
        );
        set("attribute", plain(c(86, 182, 194, Color::Cyan)));
        set("tag", plain(c(224, 108, 117, Color::Red)));
        // Bracket-match highlight (plan §1.3): emphatic ink + underline, overridable via
        // `[theme] "bracket.match"`.
        set(
            "bracket.match",
            CaptureStyle {
                fg: c(255, 255, 255, Color::White),
                modifier: Modifier::BOLD | Modifier::UNDERLINED,
            },
        );
        // Git change-bar colors (plan §4.1), overridable via `[theme]`.
        set("git.add", plain(c(87, 171, 90, Color::Green)));
        set("git.modify", plain(c(97, 175, 239, Color::Blue)));
        set("git.delete", plain(c(224, 108, 117, Color::Red)));

        Theme {
            map,
            selection_bg: c(50, 60, 90, Color::Blue),
            gutter_fg: c(92, 99, 112, Color::DarkGray),
            dark: true,
        }
    }

    /// A light theme (same capture set, darker inks on a light ground).
    pub fn default_light(truecolor: bool) -> Theme {
        let c = |r: u8, g: u8, b: u8, ansi: Color| -> Color {
            if truecolor {
                Color::Rgb(r, g, b)
            } else {
                ansi
            }
        };
        let plain = |fg: Color| CaptureStyle {
            fg,
            modifier: Modifier::empty(),
        };
        let italic = |fg: Color| CaptureStyle {
            fg,
            modifier: Modifier::ITALIC,
        };
        let mut map = HashMap::new();
        let mut set = |k: &str, v: CaptureStyle| {
            map.insert(k.to_string(), v);
        };
        set("keyword", plain(c(167, 29, 93, Color::Magenta)));
        set("function", plain(c(0, 92, 197, Color::Blue)));
        set("function.macro", plain(c(0, 134, 179, Color::Cyan)));
        set("type", plain(c(121, 94, 38, Color::Yellow)));
        set("constructor", plain(c(121, 94, 38, Color::Yellow)));
        set("string", plain(c(3, 47, 98, Color::Green)));
        set("number", plain(c(0, 92, 197, Color::Yellow)));
        set("constant", plain(c(0, 92, 197, Color::Yellow)));
        set("constant.builtin", plain(c(0, 134, 179, Color::Cyan)));
        set("comment", italic(c(106, 115, 125, Color::DarkGray)));
        set("property", plain(c(215, 58, 73, Color::Red)));
        set("variable", plain(c(36, 41, 46, Color::Black)));
        set("operator", plain(c(36, 41, 46, Color::Black)));
        set("punctuation", plain(c(36, 41, 46, Color::Black)));
        set("attribute", plain(c(0, 134, 179, Color::Cyan)));
        set(
            "bracket.match",
            CaptureStyle {
                fg: c(0, 0, 0, Color::Black),
                modifier: Modifier::BOLD | Modifier::UNDERLINED,
            },
        );
        set("git.add", plain(c(40, 140, 50, Color::Green)));
        set("git.modify", plain(c(0, 92, 197, Color::Blue)));
        set("git.delete", plain(c(200, 40, 50, Color::Red)));
        Theme {
            map,
            selection_bg: c(200, 220, 250, Color::Blue),
            gutter_fg: c(160, 165, 170, Color::DarkGray),
            dark: false,
        }
    }

    pub fn is_dark(&self) -> bool {
        self.dark
    }

    /// Load user theme overrides from `<config>/lumina/theme.toml` if present.
    pub fn load_user_overrides(&mut self) {
        if let Some(dirs) = directories::ProjectDirs::from("", "", "lumina") {
            let path = dirs.config_dir().join("theme.toml");
            if let Ok(src) = std::fs::read_to_string(&path) {
                let _ = self.apply_toml(&src);
            }
        }
    }

    /// Resolve a capture name to a style, falling back to progressively shorter prefixes
    /// (`string.special.path` → `string.special` → `string`), then to default text.
    pub fn style_for(&self, capture: &str) -> Option<Style> {
        let mut name = capture;
        loop {
            if let Some(cs) = self.map.get(name) {
                return Some(Style::default().fg(cs.fg).add_modifier(cs.modifier));
            }
            let i = name.rfind('.')?;
            name = &name[..i];
        }
    }

    /// Merge overrides parsed from a TOML `[theme]`-style table: `capture = "#rrggbb"`.
    pub fn apply_toml(&mut self, toml_src: &str) -> Result<(), String> {
        let value: toml::Value = toml_src.parse().map_err(|e| format!("{e}"))?;
        if let Some(table) = value.get("theme").and_then(|v| v.as_table()) {
            for (k, v) in table {
                if let Some(hex) = v.as_str() {
                    if let Some(color) = parse_hex(hex) {
                        self.map.insert(
                            k.clone(),
                            CaptureStyle {
                                fg: color,
                                modifier: Modifier::empty(),
                            },
                        );
                    }
                }
            }
        }
        Ok(())
    }
}

fn parse_hex(s: &str) -> Option<Color> {
    let s = s.trim_start_matches('#');
    if s.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&s[0..2], 16).ok()?;
    let g = u8::from_str_radix(&s[2..4], 16).ok()?;
    let b = u8::from_str_radix(&s[4..6], 16).ok()?;
    Some(Color::Rgb(r, g, b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_fallback_resolves() {
        let t = Theme::default_dark(true);
        // A dotted capture with no exact entry falls back to its prefix.
        assert!(t.style_for("string.special.path").is_some());
        assert!(t.style_for("keyword.control.return").is_some());
        assert!(t.style_for("totally.unknown").is_none());
    }

    #[test]
    fn toml_override_applies() {
        let mut t = Theme::default_dark(true);
        t.apply_toml("[theme]\nkeyword = \"#ff0000\"").unwrap();
        let s = t.style_for("keyword").unwrap();
        assert_eq!(s.fg, Some(Color::Rgb(255, 0, 0)));
    }
}
