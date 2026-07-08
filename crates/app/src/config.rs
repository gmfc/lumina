//! User configuration: `<config>/lumina/config.toml` (plan §6). Carries keybinding
//! overrides and a few settings. Hot-reloadable — the app rebuilds its keymap when the
//! file changes (watcher wired in Phase 8; a `config.reload` command reloads on demand).

use std::path::PathBuf;

/// Parsed configuration with sensible defaults.
pub struct Config {
    /// `(chord, command-id)` overrides layered on top of the defaults.
    pub keybindings: Vec<(String, String)>,
    pub tab_width: usize,
    pub sidebar_width: u16,
    pub follow_mode: bool,
    pub poll_watch: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            keybindings: Vec::new(),
            tab_width: 4,
            sidebar_width: 30,
            follow_mode: false,
            poll_watch: false,
        }
    }
}

impl Config {
    /// The path to the user config file, if a config dir is resolvable.
    pub fn path() -> Option<PathBuf> {
        directories::ProjectDirs::from("", "", "lumina").map(|d| d.config_dir().join("config.toml"))
    }

    /// Load from the user config file, or defaults if absent/unreadable.
    pub fn load() -> Config {
        match Config::path().and_then(|p| std::fs::read_to_string(p).ok()) {
            Some(src) => Config::from_toml_str(&src).unwrap_or_default(),
            None => Config::default(),
        }
    }

    /// Parse a config from a TOML string.
    pub fn from_toml_str(src: &str) -> Result<Config, String> {
        let value: toml::Value = src.parse().map_err(|e| format!("{e}"))?;
        let mut cfg = Config::default();

        if let Some(settings) = value.get("settings").and_then(|v| v.as_table()) {
            if let Some(n) = settings.get("tab_width").and_then(|v| v.as_integer()) {
                cfg.tab_width = n.clamp(1, 16) as usize;
            }
            if let Some(n) = settings.get("sidebar_width").and_then(|v| v.as_integer()) {
                cfg.sidebar_width = n.clamp(10, 120) as u16;
            }
            if let Some(b) = settings.get("follow_mode").and_then(|v| v.as_bool()) {
                cfg.follow_mode = b;
            }
            if let Some(b) = settings.get("poll_watch").and_then(|v| v.as_bool()) {
                cfg.poll_watch = b;
            }
        }

        if let Some(keys) = value.get("keys").and_then(|v| v.as_table()) {
            for (chord, id) in keys {
                if let Some(id) = id.as_str() {
                    cfg.keybindings.push((chord.clone(), id.to_string()));
                }
            }
        }

        Ok(cfg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_settings_and_keys() {
        let src = r#"
            [settings]
            tab_width = 2
            sidebar_width = 40
            follow_mode = true

            [keys]
            "ctrl+s" = "file.saveAll"
            "ctrl+k ctrl+w" = "tab.closeAll"
        "#;
        let cfg = Config::from_toml_str(src).unwrap();
        assert_eq!(cfg.tab_width, 2);
        assert_eq!(cfg.sidebar_width, 40);
        assert!(cfg.follow_mode);
        assert_eq!(cfg.keybindings.len(), 2);
        assert!(cfg
            .keybindings
            .iter()
            .any(|(c, i)| c == "ctrl+s" && i == "file.saveAll"));
    }
}
