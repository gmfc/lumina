//! Construction, the terminal run loop, config reload, and session save.
//!
//! Part of the [`crate::app`] module; these are `impl App` blocks split out by concern.

use super::*;

impl App {
    pub fn new(arg: Option<String>) -> Result<App> {
        let (root, open_file) = resolve_arg(arg);
        let mut editor = EditorState::new(root);
        let (config, config_error) = crate::config::Config::load();
        // Surface a malformed config instead of silently booting on defaults (§5).
        if let Some(e) = &config_error {
            editor.status_message = Some(config_parse_status(e));
        }

        // Built-in plugins + any external (script) plugins from the plugins dirs. Both
        // register through the same Registry — the external tier has no special path.
        let mut plugins = editor_builtins::all_builtins_with(config.icons);
        for dir in external_plugin_dirs(&editor.workspace.root) {
            plugins.extend(editor_plugin::runtime::load_dir(&dir));
        }
        // Honor plugins the user disabled in `[plugins]` (Settings → Plugins).
        plugins.retain(|p| config.is_plugin_enabled(p.id()));
        let mut registry = Registry::with_plugins(plugins);
        registry.activate_all(&mut editor);
        // Turn the `vim` plugin on if the config asked for it (it owns the modal state now).
        if config.vim {
            registry.dispatch_command("vim.enable", &mut editor);
        }
        // Mirror the command set onto EditorState so a palette plugin can enumerate it through
        // `Host::commands` (the registry is unreachable behind the split-borrow wall).
        editor.command_catalog = command_catalog(&registry);

        if let Some(file) = open_file {
            match files::load(&file) {
                Ok(mut doc) => {
                    doc.set_caret(0);
                    editor.workspace.open_document(doc);
                }
                Err(e) => {
                    editor.status_message = Some(format!("Could not open {}: {e}", file.display()));
                }
            }
        } else if let Some(session) = crate::session::load(&editor.workspace.root) {
            // Restore the previous session for this project root (plan §6).
            for entry in &session.files {
                if let Ok(mut doc) = files::load(&entry.path) {
                    let pos = doc.clamp(entry.cursor);
                    doc.set_caret(pos);
                    doc.view.scroll_line = entry.scroll;
                    editor.workspace.open_document(doc);
                }
            }
            editor.workspace.focus_tab(
                session
                    .active
                    .min(editor.workspace.tabs.len().saturating_sub(1)),
            );
        }

        let truecolor = crate::theme::truecolor_supported();
        let mut theme = crate::theme::Theme::default_dark(truecolor);
        theme.load_user_overrides();

        editor.sidebar_width = config.sidebar_width;
        // The terminal dock lifecycle lives in the `terminal` plugin; the app keeps the PTY
        // sessions on `EditorState`. Seed the render height + default shell from config.
        editor.terminal_height = config.terminal_height.clamp(3, 60);
        editor.terminal_shell = crate::terminal::default_shell(config.terminal_shell.as_deref());
        let follow_mode = config.follow_mode;
        let lsp = crate::lsp::LspManager::new(
            &editor.workspace.root,
            config.lsp_servers.clone(),
            env!("CARGO_PKG_VERSION").to_string(),
        );
        // Mirror LSP availability onto EditorState so the `lsp` plugin can no-op through
        // `Host::lsp_enabled` when no server is configured.
        editor.lsp_enabled = lsp.is_enabled();
        let keymap = build_keymap(&config, &registry);

        // Background worker channel + directory watcher on the project root (plan §6). Also
        // watch the config dir (non-recursively) so edits to config.toml hot-reload.
        let (worker_tx, worker_rx) = crate::worker::channel();
        // Hand the worker sender to EditorState so `Host::spawn_job` can run plugin work
        // off-thread and fold results back as `Event::JobComplete`.
        editor.job_tx = Some(worker_tx.clone());
        let config_path = crate::config::Config::path();
        let config_dir = config_path
            .as_ref()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()));
        let watcher = crate::worker::spawn_watcher(
            editor.workspace.root.clone(),
            config_dir,
            config.poll_watch,
            worker_tx.clone(),
        );
        if watcher.is_none() {
            editor.status_message =
                Some("File watching unavailable (edits won't auto-reload)".into());
        }

        Ok(App {
            editor,
            registry,
            quit: false,
            page_height: 20,
            regions: Regions::default(),
            theme,
            keymap,
            pending: Vec::new(),
            config,
            drag_anchor: None,
            tab_drag: None,
            last_click: None,
            worker_tx,
            worker_rx,
            _watcher: watcher,
            config_path,
            pending_self_writes: std::collections::HashMap::new(),
            follow_mode,
            lsp,
            lsp_sent_revision: std::collections::HashMap::new(),
            lsp_pulled_revision: std::collections::HashMap::new(),
            lsp_pull_deadline: std::collections::HashMap::new(),
            last_active: None,
            closed_tabs: Vec::new(),
            settings: None,
            settings_doc: None,
        })
    }

    /// Reload the config file and rebuild the keymap (the `config.reload` command).
    pub(super) fn reload_config(&mut self) {
        let (config, config_error) = crate::config::Config::load();
        self.config = config;
        self.editor.sidebar_width = self.config.sidebar_width;
        self.editor.terminal_height = self.config.terminal_height.clamp(3, 60);
        self.editor.terminal_shell =
            crate::terminal::default_shell(self.config.terminal_shell.as_deref());
        self.keymap = build_keymap(&self.config, &self.registry);
        // Reconcile the `vim` plugin with the reloaded config (enable is idempotent when on).
        let vim_cmd = if self.config.vim {
            "vim.enable"
        } else {
            "vim.disable"
        };
        self.registry.dispatch_command(vim_cmd, &mut self.editor);
        // Report the parse failure rather than a misleading "reloaded" when the file is
        // malformed (§5) — a typo reverts to defaults, so say so instead of lying.
        self.editor.status_message = Some(match config_error {
            Some(e) => config_parse_status(&e),
            None => "Configuration reloaded".into(),
        });
    }

    pub fn run(&mut self, terminal: &mut DefaultTerminal) -> Result<()> {
        // Prime the git gutter for any files restored at startup (plan §4.1).
        self.refresh_git_all();
        while !self.quit {
            self.editor.update_highlights(self.page_height);
            self.editor.update_bracket_match();
            self.update_lsp();
            terminal.draw(|f| ui::draw(f, self))?;
            // Reconcile each PTY's size to the panel region we just laid out.
            self.sync_terminals();

            if event::poll(Duration::from_millis(16))? {
                match event::read()? {
                    CtEvent::Key(k) if k.kind == KeyEventKind::Press => self.on_key(k),
                    CtEvent::Mouse(m) => self.on_mouse(m),
                    CtEvent::Paste(s) => self.on_paste(s),
                    CtEvent::Resize(..) => {}
                    _ => {}
                }
            }
            // Drain background worker messages (FS/LSP/parse/terminal output).
            self.drain_workers();
            self.ensure_cursor_visible();
        }
        // Graceful LSP teardown on quit: shutdown→exit→wait per server, bounded so a hung
        // server can't delay exit beyond the deadline (§3.8).
        self.lsp.stop_all(Duration::from_secs(3));
        self.save_session();
        Ok(())
    }

    /// Persist the open files + cursor/scroll for this project root (plan §6).
    pub(super) fn save_session(&self) {
        let ws = &self.editor.workspace;
        // Untitled buffers can't be restored, so only path-backed tabs are saved. `active` must
        // index that *filtered* list, not `ws.tabs` — otherwise an untitled tab sitting before the
        // active one shifts the indices and restore focuses the wrong file. Track where the active
        // tab (or the last path-backed tab at/before it) lands after the untitled tabs drop out.
        let mut files: Vec<crate::session::SessionEntry> = Vec::new();
        let mut active = 0usize;
        for (i, &id) in ws.tabs.iter().enumerate() {
            let Some(doc) = ws.documents.get(id) else {
                continue;
            };
            let Some(path) = doc.path.clone() else {
                continue;
            };
            if i <= ws.active_tab {
                active = files.len();
            }
            files.push(crate::session::SessionEntry {
                path,
                cursor: doc.selections.primary().head,
                scroll: doc.view.scroll_line,
            });
        }
        let session = crate::session::Session { files, active };
        crate::session::save(&ws.root, &session);
    }

    /// Keep the primary cursor within the viewport by adjusting the active doc's scroll,
    /// vertically (line) and horizontally (display column, for long lines).
    pub(super) fn ensure_cursor_visible(&mut self) {
        let height = self.page_height.max(1);
        // Text-area width = editor pane minus the line-number gutter. Read from the last
        // laid-out frame; 0 before the first render disables hscroll for that tick.
        let text_width = self
            .editor
            .active_document()
            .map(|doc| {
                self.regions
                    .editor
                    .width
                    .saturating_sub(ui::gutter_width(doc)) as usize
            })
            .unwrap_or(0);
        if let Some(doc) = self.editor.active_document_mut() {
            let head = doc.selections.primary().head;
            let (line, col_chars) = doc.char_to_line_col(head);
            doc.view.scroll_to_line(line, height);
            // Map the caret to a display column (tabs/wide chars expanded) and keep it in view.
            let text = doc.line_text(line);
            let body = text.trim_end_matches(['\n', '\r']);
            let display_col =
                editor_core::view::char_to_display_col(body, col_chars, doc.tab_width);
            doc.view.scroll_to_col(display_col, text_width);
        }
    }
}

/// Status line shown when the user config exists but fails to parse: the settings fall back
/// to defaults, so say the parse failed rather than pretending everything applied (§5).
fn config_parse_status(err: &str) -> String {
    format!("Config failed to parse, using defaults: {err}")
}

/// Directories to scan for external plugins: the user config dir and the project-local
/// `.lumina/plugins` folder.
fn external_plugin_dirs(root: &std::path::Path) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(pd) = directories::ProjectDirs::from("", "", "lumina") {
        dirs.push(pd.config_dir().join("plugins"));
    }
    dirs.push(root.join(".lumina").join("plugins"));
    dirs
}

/// Resolve the CLI arg into (project root, optional file to open).
fn resolve_arg(arg: Option<String>) -> (PathBuf, Option<PathBuf>) {
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    match arg {
        None => (cwd, None),
        Some(a) => {
            let p = PathBuf::from(&a);
            if p.is_dir() {
                (p, None)
            } else if p.is_file() {
                let root = p.parent().map(|x| x.to_path_buf()).unwrap_or(cwd);
                (root, Some(p))
            } else {
                // Non-existent path: treat as a new file under cwd.
                (cwd, Some(p))
            }
        }
    }
}
