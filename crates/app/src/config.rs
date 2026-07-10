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
    /// Auto-close typed brackets/quotes and type over / delete-both (plan §1.1).
    pub auto_pairs: bool,
    /// Copy indent (and adjust for brackets) on newline; dedent on closing bracket (plan §1.2).
    pub auto_indent: bool,
    /// On save, strip trailing whitespace from every line (plan §1.4). Off by default to
    /// respect the "never silently rewrite" invariant.
    pub trim_trailing_whitespace: bool,
    /// On save, ensure the file ends with a single newline (plan §1.4). Off by default.
    pub insert_final_newline: bool,
    /// Show a per-line git change bar in the gutter (plan §4.1).
    pub git_gutter: bool,
    /// Show Nerd Font file-type glyphs in the explorer (off → ASCII `▸ ▾` markers).
    pub icons: bool,
    /// Start in Vim modal-editing mode (Normal/Insert/Visual). Off by default —
    /// lumina is mouse-first; opt in with `vim = true`.
    pub vim: bool,
    /// Shell for the integrated terminal (program + optional args). `None` → the platform
    /// default (`$SHELL` / `/bin/sh`, or `%ComSpec%` / `cmd.exe` on Windows).
    pub terminal_shell: Option<String>,
    /// Content height (rows) of the terminal panel when expanded.
    pub terminal_height: u16,
    /// `language → server command (split into program + args)`.
    pub lsp_servers: std::collections::HashMap<String, Vec<String>>,
    /// Plugin ids the user has disabled (`[plugins] "<id>" = false`). Applied when
    /// plugins load, so the change takes effect on the next launch / config reload.
    pub disabled_plugins: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            keybindings: Vec::new(),
            tab_width: 4,
            sidebar_width: 30,
            follow_mode: false,
            poll_watch: false,
            auto_pairs: true,
            auto_indent: true,
            trim_trailing_whitespace: false,
            insert_final_newline: false,
            git_gutter: true,
            icons: false,
            vim: false,
            terminal_shell: None,
            terminal_height: 12,
            lsp_servers: std::collections::HashMap::new(),
            disabled_plugins: Vec::new(),
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
            cfg.apply_settings(settings);
        }
        if let Some(keys) = value.get("keys").and_then(|v| v.as_table()) {
            cfg.apply_keys(keys);
        }
        if let Some(lsp) = value.get("lsp").and_then(|v| v.as_table()) {
            cfg.apply_lsp(lsp);
        }
        if let Some(plugins) = value.get("plugins").and_then(|v| v.as_table()) {
            for (id, enabled) in plugins {
                if enabled.as_bool() == Some(false) {
                    cfg.disabled_plugins.push(id.clone());
                }
            }
        }

        Ok(cfg)
    }

    /// True unless the user disabled the plugin `id` in `[plugins]`.
    pub fn is_plugin_enabled(&self, id: &str) -> bool {
        !self.disabled_plugins.iter().any(|d| d == id)
    }

    /// Serialize the current settings back to `path`, preserving any `[keys]`,
    /// `[lsp]`, and `[theme]` sections already there (the `[settings]` and
    /// `[plugins]` tables are rewritten from this value). Comments are not
    /// preserved — `toml` has no formatting round-trip.
    pub fn write_to(&self, path: &std::path::Path) -> Result<(), String> {
        // Start from the existing file so unmanaged sections survive.
        let mut root: toml::Table = std::fs::read_to_string(path)
            .ok()
            .and_then(|s| s.parse::<toml::Table>().ok())
            .unwrap_or_default();

        let mut settings = toml::Table::new();
        let entries: [(&str, toml::Value); 12] = [
            ("tab_width", (self.tab_width as i64).into()),
            ("sidebar_width", (self.sidebar_width as i64).into()),
            ("follow_mode", self.follow_mode.into()),
            ("poll_watch", self.poll_watch.into()),
            ("auto_pairs", self.auto_pairs.into()),
            ("auto_indent", self.auto_indent.into()),
            (
                "trim_trailing_whitespace",
                self.trim_trailing_whitespace.into(),
            ),
            ("insert_final_newline", self.insert_final_newline.into()),
            ("git_gutter", self.git_gutter.into()),
            ("icons", self.icons.into()),
            ("vim", self.vim.into()),
            ("terminal_height", (self.terminal_height as i64).into()),
        ];
        for (k, v) in entries {
            settings.insert(k.to_string(), v);
        }
        if let Some(shell) = &self.terminal_shell {
            settings.insert("terminal_shell".to_string(), shell.clone().into());
        }
        root.insert("settings".to_string(), settings.into());

        if self.disabled_plugins.is_empty() {
            root.remove("plugins");
        } else {
            let mut plugins = toml::Table::new();
            for id in &self.disabled_plugins {
                plugins.insert(id.clone(), false.into());
            }
            root.insert("plugins".to_string(), plugins.into());
        }

        let out = toml::to_string_pretty(&root).map_err(|e| e.to_string())?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(path, out).map_err(|e| e.to_string())
    }

    /// Merge the `[settings]` table, clamping numeric fields to sane ranges.
    fn apply_settings(&mut self, settings: &toml::Table) {
        if let Some(n) = settings.get("tab_width").and_then(|v| v.as_integer()) {
            self.tab_width = n.clamp(1, 16) as usize;
        }
        if let Some(n) = settings.get("sidebar_width").and_then(|v| v.as_integer()) {
            self.sidebar_width = n.clamp(10, 120) as u16;
        }
        if let Some(b) = settings.get("follow_mode").and_then(|v| v.as_bool()) {
            self.follow_mode = b;
        }
        if let Some(b) = settings.get("poll_watch").and_then(|v| v.as_bool()) {
            self.poll_watch = b;
        }
        if let Some(b) = settings.get("auto_pairs").and_then(|v| v.as_bool()) {
            self.auto_pairs = b;
        }
        if let Some(b) = settings.get("auto_indent").and_then(|v| v.as_bool()) {
            self.auto_indent = b;
        }
        if let Some(b) = settings
            .get("trim_trailing_whitespace")
            .and_then(|v| v.as_bool())
        {
            self.trim_trailing_whitespace = b;
        }
        if let Some(b) = settings
            .get("insert_final_newline")
            .and_then(|v| v.as_bool())
        {
            self.insert_final_newline = b;
        }
        if let Some(b) = settings.get("git_gutter").and_then(|v| v.as_bool()) {
            self.git_gutter = b;
        }
        if let Some(b) = settings.get("icons").and_then(|v| v.as_bool()) {
            self.icons = b;
        }
        if let Some(b) = settings.get("vim").and_then(|v| v.as_bool()) {
            self.vim = b;
        }
        if let Some(s) = settings.get("terminal_shell").and_then(|v| v.as_str()) {
            let s = s.trim();
            if !s.is_empty() {
                self.terminal_shell = Some(s.to_string());
            }
        }
        if let Some(n) = settings.get("terminal_height").and_then(|v| v.as_integer()) {
            self.terminal_height = n.clamp(3, 60) as u16;
        }
    }

    /// Merge the `[keys]` table of `chord -> command-id` overrides.
    fn apply_keys(&mut self, keys: &toml::Table) {
        for (chord, id) in keys {
            if let Some(id) = id.as_str() {
                self.keybindings.push((chord.clone(), id.to_string()));
            }
        }
    }

    /// Merge the `[lsp]` table of `language -> server command line`.
    fn apply_lsp(&mut self, lsp: &toml::Table) {
        for (lang, cmd) in lsp {
            if let Some(cmd) = cmd.as_str() {
                let parts: Vec<String> = cmd.split_whitespace().map(String::from).collect();
                if !parts.is_empty() {
                    self.lsp_servers.insert(lang.clone(), parts);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_lsp_servers() {
        let src = r#"
            [lsp]
            rust = "rust-analyzer"
            python = "pylsp --stdio"
        "#;
        let cfg = Config::from_toml_str(src).unwrap();
        assert_eq!(cfg.lsp_servers["rust"], vec!["rust-analyzer"]);
        assert_eq!(cfg.lsp_servers["python"], vec!["pylsp", "--stdio"]);
    }

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

    #[test]
    fn write_to_roundtrips_and_preserves_other_sections() {
        let mut dir = std::env::temp_dir();
        dir.push(format!("lumina_cfg_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        // Seed a [keys] section the writer must preserve.
        std::fs::write(&path, "[keys]\n\"ctrl+k ctrl+u\" = \"shout.line\"\n").unwrap();

        let cfg = Config {
            vim: true,
            tab_width: 2,
            auto_pairs: false,
            terminal_shell: Some("/bin/zsh".into()),
            disabled_plugins: vec!["todo".into()],
            ..Config::default()
        };
        cfg.write_to(&path).unwrap();

        let raw = std::fs::read_to_string(&path).unwrap();
        let reread = Config::from_toml_str(&raw).unwrap();
        assert!(reread.vim);
        assert_eq!(reread.tab_width, 2);
        assert!(!reread.auto_pairs);
        assert_eq!(reread.terminal_shell.as_deref(), Some("/bin/zsh"));
        assert!(reread.disabled_plugins.iter().any(|d| d == "todo"));
        assert!(!reread.is_plugin_enabled("todo"));
        assert!(reread.is_plugin_enabled("explorer"));
        // The unmanaged [keys] section survived the rewrite.
        assert!(raw.contains("shout.line"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn parses_vim_setting() {
        assert!(!Config::default().vim);
        let cfg = Config::from_toml_str("[settings]\nvim = true").unwrap();
        assert!(cfg.vim);
        let cfg = Config::from_toml_str("[settings]\nvim = false").unwrap();
        assert!(!cfg.vim);
    }

    #[test]
    fn parses_terminal_settings() {
        let src = r#"
            [settings]
            terminal_shell = "/bin/zsh -l"
            terminal_height = 20
        "#;
        let cfg = Config::from_toml_str(src).unwrap();
        assert_eq!(cfg.terminal_shell.as_deref(), Some("/bin/zsh -l"));
        assert_eq!(cfg.terminal_height, 20);

        // Out-of-range height is clamped; blank shell falls back to the default.
        let cfg = Config::from_toml_str("[settings]\nterminal_height = 500\nterminal_shell = \"\"")
            .unwrap();
        assert_eq!(cfg.terminal_height, 60);
        assert_eq!(cfg.terminal_shell, None);
    }
}
