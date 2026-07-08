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

        Theme {
            map,
            selection_bg: c(50, 60, 90, Color::Blue),
            gutter_fg: c(92, 99, 112, Color::DarkGray),
        }
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
            match name.rfind('.') {
                Some(i) => name = &name[..i],
                None => return None,
            }
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
