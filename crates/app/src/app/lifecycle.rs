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
        // Soft word-wrap default (Alt+Z toggles at runtime). Mirrored onto docs on the first
        // viewport clamp; if on at startup, seed any restored docs now so frame 1 wraps.
        editor.wrap_enabled = config.line_wrap;
        if config.line_wrap {
            for doc in editor.workspace.documents.values_mut() {
                doc.view.wrap = true;
            }
        }
        // The terminal dock lifecycle lives in the `terminal` plugin; the app keeps the PTY
        // sessions on `EditorState`. Seed the render height + default shell from config.
        editor.terminal_height = config.terminal_height.clamp(3, 60);
        editor.terminal_shell = crate::terminal::default_shell(config.terminal_shell.as_deref());
        let follow_mode = config.follow_mode;
        // Root the servers at the nearest project marker (walk up for Cargo.toml/.git/…), so an
        // LSP started from a file opened deep in a tree still sees the workspace manifest.
        let mut lsp = crate::lsp::LspManager::new(
            &crate::files::project_root(&editor.workspace.root),
            config.lsp_servers.clone(),
            env!("CARGO_PKG_VERSION").to_string(),
        );
        // Zero-config discovery: with no `[lsp]` override, probe `$PATH` for a known server per
        // language on demand (the built-in registry). Off in tests to stay hermetic.
        lsp.enable_discovery();
        // Mirror LSP availability onto EditorState so the `lsp` plugin can no-op through
        // `Host::lsp_enabled` when the layer is off.
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
            lsp_autoopened: std::collections::HashSet::new(),
            last_caret: None,
            force_redraw: true,
            last_frame_sig: None,
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

    /// Re-clamp the viewport to the caret **only when the caret (or active doc) moved** since the
    /// last clamp. A plain wheel-scroll moves only `scroll_line`, leaving the caret put — without
    /// this gate, [`Self::ensure_cursor_visible`] would snap the view back to the caret every tick,
    /// so scrolling would stall the moment the caret reached the viewport edge.
    pub(super) fn refresh_viewport(&mut self) {
        // Keep the active doc's wrap geometry current **every frame** — a terminal resize changes
        // the pane width without moving the caret, and motions/clicks read `view.wrap_width`. (The
        // renderer wraps at the live pane width directly, so it is already resize-correct.)
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
        let wrap_enabled = self.editor.wrap_enabled;
        if let Some(doc) = self.editor.active_document_mut() {
            doc.view.wrap = wrap_enabled;
            doc.view.wrap_width = text_width;
        }
        let cur = self.editor.workspace.active_doc().and_then(|id| {
            self.editor
                .workspace
                .documents
                .get(id)
                .map(|d| (id, d.selections.primary().head))
        });
        if cur != self.last_caret {
            self.ensure_cursor_visible();
            self.last_caret = cur;
        }
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
        let wrap_enabled = self.editor.wrap_enabled;
        if let Some(doc) = self.editor.active_document_mut() {
            // Mirror the app-wide wrap flag onto the active doc (covers docs opened after a toggle),
            // and keep the pane width current so wrap-aware motions/rendering see the geometry.
            doc.view.wrap = wrap_enabled;
            doc.view.wrap_width = text_width;
            let head = doc.selections.primary().head;
            if doc.view.wrap && text_width > 0 {
                // Soft-wrap: no horizontal scroll; clamp the *visual-row* anchor to the caret.
                doc.view.scroll_col = 0;
                let tab = doc.tab_width;
                let (sl, ss) = editor_core::view::wrapped_scroll_anchor(
                    doc,
                    head,
                    height,
                    text_width,
                    tab,
                    doc.view.scroll_line,
                    doc.view.scroll_sub,
                );
                doc.view.scroll_line = sl;
                doc.view.scroll_sub = ss;
                return;
            }
            let (line, col_chars) = doc.char_to_line_col(head);
            doc.view.scroll_sub = 0;
            doc.view.scroll_to_line(line, height);
            // Map the caret to a display column (tabs/wide chars expanded) and keep it in view.
            let text = doc.line_str(line);
            let body = text.trim_end_matches(['\n', '\r']);
            let display_col =
                editor_core::view::char_to_display_col(body, col_chars, doc.tab_width);
            doc.view.scroll_to_col(display_col, text_width);
        }
    }

    /// Toggle app-wide soft word-wrap (`view.toggleWrap` / Alt+Z). Wrap is global, so the new state
    /// is mirrored onto **every** open document; the caret itself didn't move, so a re-clamp is
    /// forced next tick to re-anchor the viewport for the new mode.
    pub(super) fn toggle_wrap(&mut self) {
        let on = !self.editor.wrap_enabled;
        self.editor.wrap_enabled = on;
        for doc in self.editor.workspace.documents.values_mut() {
            doc.view.wrap = on;
            if !on {
                doc.view.scroll_sub = 0;
                doc.view.scroll_col = 0;
            }
        }
        self.last_caret = None;
        self.editor.status_message = Some(if on {
            "Word wrap: on".into()
        } else {
            "Word wrap: off".into()
        });
    }

    /// Cheap editor-pane fingerprint for the idle-frame gate: the active doc, its revision, the
    /// primary caret, and the scroll offsets. A change in any of these means the editor pane would
    /// render differently, so the frame must repaint (see [`Self::run`]).
    pub(super) fn frame_sig(&self) -> FrameSig {
        let active = self.editor.workspace.active_doc();
        match active.and_then(|id| self.editor.workspace.documents.get(id)) {
            Some(d) => (
                active,
                d.revision,
                d.selections.primary().head,
                d.view.scroll_line,
                d.view.scroll_col,
            ),
            None => (active, 0, 0, 0, 0),
        }
    }

    /// Whether the footer is showing an animated spinner this frame — the LSP "starting" indicator
    /// or a work-done progress line. Both advance off the wall clock (see `ui::chrome::spinner_frame`),
    /// so while either is present the frame must keep repainting or the spinner freezes. Read from
    /// the same `status_items` the renderer uses, so this can never disagree with what's on screen.
    pub(super) fn is_animating(&self) -> bool {
        let si = &self.editor.status_items;
        si.get("lsp.health").map(String::as_str) == Some("starting")
            || si.get("lsp.progress").is_some_and(|s| !s.is_empty())
    }

    /// Whether a crashed server that an **open document** actually uses is waiting out its restart
    /// backoff. The manager arms a restart on any crash, but only open-doc languages have anything
    /// (`update_lsp`'s doc loop) to drive `ensure_started` and clear the backoff — so a server that
    /// crashed after its last doc closed must be excluded, or its never-cleared `restart_after`
    /// would pin the idle loop awake forever.
    pub(super) fn lsp_restart_pending(&self) -> bool {
        let mut armed = self.lsp.restart_langs().peekable();
        if armed.peek().is_none() {
            return false; // no crashed server waiting — the common case, no set to build
        }
        let open: std::collections::HashSet<&str> = self
            .editor
            .workspace
            .documents
            .values()
            .filter_map(|d| d.language.as_deref())
            .collect();
        self.lsp.restart_langs().any(|lang| open.contains(lang))
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
            // Absolutize up front so the workspace root (→ `rootUri`) and the opened file both yield
            // well-formed `file://` URIs; a relative arg otherwise produces `file://rel/...`.
            let p = crate::files::absolute_path(std::path::Path::new(&a));
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
