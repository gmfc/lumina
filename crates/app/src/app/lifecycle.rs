//! Construction, the terminal run loop, config reload, and session save.
//!
//! Part of the [`crate::app`] module; these are `impl App` blocks split out by concern.

use super::*;

impl App {
    pub fn new(arg: Option<String>) -> Result<App> {
        let (root, open_file) = resolve_arg(arg);
        let mut editor = EditorState::new(root);
        let config = crate::config::Config::load();

        // Built-in plugins + any external (script) plugins from the plugins dirs. Both
        // register through the same Registry — the external tier has no special path.
        let mut plugins = editor_builtins::all_builtins_with(config.icons);
        for dir in external_plugin_dirs(&editor.workspace.root) {
            plugins.extend(editor_plugin::runtime::load_dir(&dir));
        }
        let mut registry = Registry::with_plugins(plugins);
        registry.activate_all(&mut editor);

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
        let panel = crate::terminal::TerminalPanel::new(config.terminal_height);
        let follow_mode = config.follow_mode;
        let lsp = crate::lsp::LspManager::new(&editor.workspace.root, config.lsp_servers.clone());
        let keymap = build_keymap(&config);

        // Background worker channel + directory watcher on the project root (plan §6). Also
        // watch the config dir (non-recursively) so edits to config.toml hot-reload.
        let (worker_tx, worker_rx) = crate::worker::channel();
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
            clipboard: crate::clipboard::Clipboard::new(),
            drag_anchor: None,
            tab_drag: None,
            last_click: None,
            worker_tx,
            worker_rx,
            _watcher: watcher,
            config_path,
            pending_self_writes: std::collections::HashMap::new(),
            follow_mode,
            search: None,
            last_search_run: String::new(),
            lsp,
            lsp_sent_revision: std::collections::HashMap::new(),
            panel,
            closed_tabs: Vec::new(),
        })
    }

    /// The active project-search state (read by the renderer).
    pub fn search(&self) -> Option<&crate::search::SearchState> {
        self.search.as_ref()
    }

    /// Reload the config file and rebuild the keymap (the `config.reload` command).
    pub(super) fn reload_config(&mut self) {
        self.config = crate::config::Config::load();
        self.editor.sidebar_width = self.config.sidebar_width;
        self.panel.height = self.config.terminal_height.clamp(3, 60);
        self.keymap = build_keymap(&self.config);
        self.editor.status_message = Some("Configuration reloaded".into());
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
        self.save_session();
        Ok(())
    }

    /// Persist the open files + cursor/scroll for this project root (plan §6).
    pub(super) fn save_session(&self) {
        let ws = &self.editor.workspace;
        let files: Vec<crate::session::SessionEntry> = ws
            .tabs
            .iter()
            .filter_map(|&id| ws.documents.get(id))
            .filter_map(|doc| {
                doc.path.clone().map(|path| crate::session::SessionEntry {
                    path,
                    cursor: doc.selections.primary().head,
                    scroll: doc.view.scroll_line,
                })
            })
            .collect();
        let session = crate::session::Session {
            files,
            active: ws.active_tab,
        };
        crate::session::save(&ws.root, &session);
    }

    /// Keep the primary cursor within the viewport by adjusting the active doc's scroll.
    pub(super) fn ensure_cursor_visible(&mut self) {
        let height = self.page_height.max(1);
        if let Some(doc) = self.editor.active_document_mut() {
            let head = doc.selections.primary().head;
            let line = doc.char_to_line(head);
            doc.view.scroll_to_line(line, height);
        }
    }
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
