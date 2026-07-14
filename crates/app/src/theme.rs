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
                // Best-effort: a malformed theme.toml just leaves the built-in theme in place
                // (purely cosmetic, non-fatal). This runs at startup with only the `Theme` in
                // hand — no editor/status channel to surface to and no logger (§5) — so the
                // parse error is deliberately dropped rather than aborting the boot over colors.
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

    /// Resolve a decoration layer's semantic `style` key to a concrete [`Style`] the renderer
    /// patches onto a cell. Most keys map through [`Self::style_for`] (fg + modifier); a few
    /// editor decorations need an effect the capture map can't carry (a background tint), so
    /// they're handled explicitly here — keeping all color policy in the theme while plugins
    /// speak only in semantic keys.
    pub fn decoration_style(&self, key: &str) -> Style {
        // Diagnostic severity → color, matching the former `util::severity_color`.
        let sev = |c: Color| Style::default().fg(c);
        match key {
            // The find-match highlight is a background tint; the capture map only carries fg, so
            // it lives here. Matches the former hardcoded `CLR_MATCH`.
            "find.match" => Style::default().bg(Color::Rgb(90, 74, 30)),
            // Diagnostic underlines (inline spans): severity ink + underline.
            "lsp.diag.error" => sev(Color::Red).add_modifier(Modifier::UNDERLINED),
            "lsp.diag.warning" => sev(Color::Yellow).add_modifier(Modifier::UNDERLINED),
            "lsp.diag.info" => sev(Color::Blue).add_modifier(Modifier::UNDERLINED),
            "lsp.diag.hint" => sev(Color::DarkGray).add_modifier(Modifier::UNDERLINED),
            // Diagnostic gutter markers: severity ink, no underline on the glyph.
            "lsp.diag.mark.error" => sev(Color::Red),
            "lsp.diag.mark.warning" => sev(Color::Yellow),
            "lsp.diag.mark.info" => sev(Color::Blue),
            "lsp.diag.mark.hint" => sev(Color::DarkGray),
            // Document-highlight occurrences: a background tint, brighter for a write.
            "lsp.highlight.text" | "lsp.highlight.read" => {
                Style::default().bg(Color::Rgb(58, 58, 78))
            }
            "lsp.highlight.write" => Style::default().bg(Color::Rgb(82, 62, 62)),
            // Inlay hints (§7.2): dim virtual text; parameter names a touch dimmer than type hints.
            "lsp.inlay.type" => Style::default().fg(Color::Rgb(120, 130, 145)),
            "lsp.inlay.param" => Style::default()
                .fg(Color::Rgb(110, 118, 130))
                .add_modifier(Modifier::ITALIC),
            // Code lens (§6.4): dim italic virtual text, distinct from inlay hints.
            "lsp.lens" => Style::default()
                .fg(Color::Rgb(92, 120, 140))
                .add_modifier(Modifier::ITALIC),
            // Folding (§7.3): a dim gutter chevron marking a foldable region's start.
            "lsp.fold" => Style::default().fg(Color::Rgb(110, 118, 130)),
            _ => self.style_for(key).unwrap_or_default(),
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
    // Require exactly six ASCII hex digits. Verifying ASCII before the fixed-offset slices keeps
    // them on char boundaries — a 6-*byte* multibyte string (e.g. "aébcd") would otherwise slice
    // mid-codepoint and panic (crashing the editor on a theme typo).
    if s.len() != 6 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
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

    #[test]
    fn multibyte_hex_value_is_rejected_not_panicked() {
        // "aébcd" is 6 bytes but not 6 ASCII hex digits; parse_hex must reject it, not slice
        // through the middle of 'é' and panic.
        assert_eq!(parse_hex("aébcd"), None);
        assert_eq!(parse_hex("gggggg"), None);
        assert_eq!(parse_hex("ff00ff"), Some(Color::Rgb(255, 0, 255)));
        // A malformed value in a real theme file must not crash the loader.
        let mut t = Theme::default_dark(true);
        assert!(t.apply_toml("[theme]\nkeyword = \"aébcd\"").is_ok());
    }
}
