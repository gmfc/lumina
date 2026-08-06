//! File and tab lifecycle: open, close, save / save-as, and new file.
//!
//! Part of the [`crate::app`] module; these are `impl App` blocks split out by concern.

use super::*;

impl App {
    /// Drop all per-document state for a closed document, so these maps don't grow unbounded
    /// over a long session of opening and closing files.
    pub(super) fn forget_doc(&mut self, id: editor_core::DocId) {
        self.editor.highlighters.remove(&id);
        self.editor.decorations.remove(&id);
        self.editor.tab_views.remove(&id);
        // The `diagnostics` plugin prunes its own model on tab change (DidChangeActive).
        self.editor.git_hunks.remove(&id);
        self.lsp_sent_revision.remove(&id);
        self.lsp_pulled_revision.remove(&id);
        self.lsp_pull_deadline.remove(&id);
    }

    /// Close the tab at `idx` and drop the removed document's per-doc state (see [`forget_doc`]).
    pub(super) fn close_and_forget(&mut self, idx: usize) {
        // Capture the doc's LSP identity before the tab is removed, so we can tell the server the
        // document closed (§4.1) once we confirm it was actually dropped.
        let closing = self
            .editor
            .workspace
            .tabs
            .get(idx)
            .copied()
            .and_then(|id| self.editor.workspace.documents.get(id))
            .and_then(|d| Some((d.path.clone()?, d.language.clone()?)));
        if let Some(id) = self.editor.workspace.close_tab(idx) {
            if let Some((path, lang)) = closing {
                self.lsp.did_close(&path, &lang);
            }
            self.forget_doc(id);
        }
    }

    /// Close a tab, prompting first if it has unsaved changes (plan §6).
    pub(super) fn request_close(&mut self, tab: usize) {
        let dirty = self
            .editor
            .workspace
            .tabs
            .get(tab)
            .and_then(|&id| self.editor.workspace.documents.get(id))
            .map(|d| d.dirty)
            .unwrap_or(false);
        if dirty {
            self.editor.overlay = Some(crate::editor::Overlay::ConfirmClose { tab });
        } else {
            self.remember_closed(tab);
            self.close_and_forget(tab);
        }
    }

    /// Push a closed tab's path onto the reopen stack (Ctrl+Shift+T restores the newest).
    /// Untitled buffers have no path, so nothing is remembered for them.
    pub(super) fn remember_closed(&mut self, tab: usize) {
        if let Some(&id) = self.editor.workspace.tabs.get(tab) {
            if let Some(path) = self
                .editor
                .workspace
                .documents
                .get(id)
                .and_then(|d| d.path.clone())
            {
                self.closed_tabs.push(path);
            }
        }
    }

    /// Ctrl+Shift+T: reopen the most recently closed tab that still exists and isn't already
    /// open, focusing it. Skips missing files and duplicates, popping until one lands.
    pub(super) fn reopen_closed_tab(&mut self) {
        while let Some(path) = self.closed_tabs.pop() {
            if let Some(id) = self.editor.workspace.find_by_path(&path) {
                self.editor.workspace.focus_doc(id);
                self.editor.focus = Focus::Editor;
                return;
            }
            if path.exists() {
                self.open_path(&path);
                self.editor.focus = Focus::Editor;
                return;
            }
        }
        self.editor.notify_info("No closed editors to reopen");
    }

    /// `app.quit`: the same guard `tab.close` and `tab.closeAll` already apply, on the one path
    /// that used to skip it. Session restore persists paths, cursors, and scroll — not buffer
    /// contents — so quitting past a dirty buffer loses the work outright.
    pub(super) fn request_quit(&mut self) {
        let dirty = self.dirty_tabs();
        if dirty.is_empty() {
            self.quit = true;
        } else {
            self.editor.overlay = Some(crate::editor::Overlay::ConfirmQuit { dirty });
        }
    }

    /// Tab indices with unsaved changes. Notice/viewer tabs are excluded: their buffer is an empty
    /// placeholder that holds no work to lose.
    pub(super) fn dirty_tabs(&self) -> Vec<usize> {
        self.editor
            .workspace
            .tabs
            .iter()
            .enumerate()
            .filter(|(_, &id)| !self.editor.is_tab_view(id))
            .filter(|(_, &id)| {
                self.editor
                    .workspace
                    .documents
                    .get(id)
                    .is_some_and(|d| d.dirty)
            })
            .map(|(i, _)| i)
            .collect()
    }

    /// The file names behind `tabs`, for a confirm box that names what is at risk.
    pub(crate) fn tab_names(&self, tabs: &[usize]) -> Vec<String> {
        tabs.iter()
            .filter_map(|&t| self.editor.workspace.tabs.get(t))
            .filter_map(|&id| self.editor.workspace.documents.get(id))
            .map(|d| {
                d.path
                    .as_deref()
                    .map(display_name)
                    .unwrap_or_else(|| "untitled".to_string())
            })
            .collect()
    }

    /// Save every dirty tab and quit — the `[S]` outcome of the quit confirmation. An untitled
    /// buffer has nowhere to be written, so it *cancels* the quit and opens Save As instead of
    /// being silently dropped by a command the user believed would save everything.
    pub(super) fn save_all_and_quit(&mut self) {
        if let Some(&tab) = self.dirty_tabs().iter().find(|&&t| self.tab_has_no_path(t)) {
            self.editor.workspace.focus_tab(tab);
            self.open_save_as();
            self.editor
                .notify_warn("This buffer has no file yet — name it, then quit again");
            return;
        }
        self.save_all();
        // A failed write leaves the buffer dirty; quitting anyway would discard exactly the work
        // the user asked to save.
        if self.dirty_tabs().is_empty() {
            self.quit = true;
        } else {
            self.editor
                .notify_error("Some files could not be saved — quit cancelled");
        }
    }

    /// Whether the tab at `idx` is a path-less (untitled) buffer.
    fn tab_has_no_path(&self, idx: usize) -> bool {
        self.editor
            .workspace
            .tabs
            .get(idx)
            .and_then(|&id| self.editor.workspace.documents.get(id))
            .is_some_and(|d| d.path.is_none())
    }

    /// `file.reloadFromDisk`: throw the buffer away and re-read the file. This is the exit from an
    /// external-edit conflict that keeps the other writer's version. It can't be undone (the
    /// reload drops undo history along with the text), so a dirty buffer is confirmed first.
    pub(super) fn reload_from_disk(&mut self) {
        let Some(id) = self.editor.workspace.active_doc() else {
            return;
        };
        if self.editor.is_tab_view(id) {
            self.editor
                .notify_warn("This tab isn't a text buffer — nothing to reload");
            return;
        }
        let Some(doc) = self.editor.workspace.documents.get(id) else {
            return;
        };
        if doc.path.is_none() {
            self.editor
                .notify_warn("This buffer has no file on disk to reload from");
            return;
        }
        if doc.dirty {
            self.editor.overlay = Some(crate::editor::Overlay::ConfirmReload);
            return;
        }
        self.reload_from_disk_now();
    }

    /// Perform the reload, past any confirmation: drop the local edits, re-read the file through
    /// the same open policy a fresh open uses, and clear the conflict flag.
    pub(super) fn reload_from_disk_now(&mut self) {
        let Some(id) = self.editor.workspace.active_doc() else {
            return;
        };
        let Some(path) = self
            .editor
            .workspace
            .documents
            .get(id)
            .and_then(|d| d.path.clone())
        else {
            return;
        };
        // The full open policy, not a bare read: a file that turned binary or grew past the
        // ceiling while it was open must not be slurped onto the UI thread through this door
        // any more than through `open_path`.
        match files::open(&path, &self.config.file_limits()) {
            Ok(files::Opened::Refused(refusal)) => {
                self.editor.notify_error(format!(
                    "{} can't be reloaded as text: {} — the buffer is unchanged",
                    display_name(&path),
                    refusal.label()
                ));
            }
            Ok(files::Opened::Text(fresh)) => {
                let text = fresh.to_string();
                let (encoding, line_ending, large) =
                    (fresh.encoding, fresh.line_ending, fresh.large);
                let Some(doc) = self.editor.workspace.documents.get_mut(id) else {
                    return;
                };
                doc.reload_from_str(&text);
                doc.encoding = encoding;
                doc.line_ending = line_ending;
                doc.large = large;
                doc.disk = files::fingerprint(text.as_bytes());
                doc.dirty = false;
                doc.deleted_on_disk = false;
                doc.external_conflict = None;
                doc.externally_reloaded = true;
                let caret = doc.clamp(doc.selections.primary().head);
                doc.set_caret(caret);
                // The old highlighter is keyed to the replaced text; drop it so it re-parses.
                self.editor.highlighters.remove(&id);
                self.editor.notify_info(format!(
                    "{} reloaded from disk — local changes and undo history for this file were discarded",
                    display_name(&path)
                ));
                self.editor
                    .emit(editor_plugin::event::Event::ExternalReload(id));
                self.request_git_status(id);
            }
            Err(e) => {
                let msg = io_reason(&path, &e);
                self.editor.notify_error(format!("Reload failed: {msg}"));
            }
        }
    }

    /// `file.keepMine`: accept the buffer as the truth and clear the conflict. The next save
    /// writes over the on-disk change, which is now what the user has explicitly chosen.
    pub(super) fn keep_mine(&mut self) {
        let Some(id) = self.editor.workspace.active_doc() else {
            return;
        };
        let name = self
            .editor
            .workspace
            .documents
            .get(id)
            .and_then(|d| d.path.as_deref())
            .map(display_name);
        let Some(doc) = self.editor.workspace.documents.get_mut(id) else {
            return;
        };
        let Some(fp) = doc.external_conflict.take() else {
            self.editor
                .notify_info("This file has no unresolved external change");
            return;
        };
        // Adopt the on-disk fingerprint so the watcher stops re-reporting the same change; the
        // buffer stays dirty, so the next save writes our version over it.
        doc.disk = fp;
        let name = name.unwrap_or_else(|| "This buffer".to_string());
        self.editor.notify_info(format!(
            "Keeping your version of {name} — saving will overwrite the change on disk"
        ));
    }

    /// Ctrl+K S: save every open, path-backed tab that has unsaved changes.
    pub(super) fn save_all(&mut self) {
        let restore = self.editor.workspace.active_tab;
        let count = self.editor.workspace.tabs.len();
        let mut saved = 0;
        let mut failed = 0;
        for i in 0..count {
            self.editor.workspace.focus_tab(i);
            // A notice/viewer tab has a path but no text — writing its empty placeholder would
            // truncate the very file it is displaying.
            if self
                .editor
                .workspace
                .active_doc()
                .is_some_and(|id| self.editor.is_tab_view(id))
            {
                continue;
            }
            let (has_path, dirty) = self
                .editor
                .active_document()
                .map(|d| (d.path.is_some(), d.dirty))
                .unwrap_or((false, false));
            if has_path && dirty {
                self.save_active();
                // `save_active` clears `dirty` only on a successful write, so this counts what
                // actually reached the disk rather than what was attempted.
                let ok = self.editor.active_document().is_some_and(|d| !d.dirty);
                if ok {
                    saved += 1;
                } else {
                    failed += 1;
                }
            }
        }
        self.editor.workspace.focus_tab(restore);
        // A per-file error published inside the loop can be superseded by the next file's success
        // notice, so the summary — the message that survives — has to carry the bad news itself.
        if failed > 0 {
            self.editor.notify_error(format!(
                "Saved {saved} file(s); {failed} could not be saved (see {})",
                self.notifications_hint()
            ));
        } else {
            self.editor.notify_info(format!("Saved {saved} file(s)"));
        }
    }

    /// How to reach the notice scrollback, named by its live chord when it has one.
    pub(super) fn notifications_hint(&self) -> String {
        match self.keymap.binding_label("view.notifications") {
            Some(chord) => format!("{chord} for details"),
            None => "View: Show Notifications".to_string(),
        }
    }

    /// Ctrl+K Ctrl+W: close every tab. Clean tabs close outright; the first dirty one opens
    /// the confirm-close prompt and stops, so no unsaved work is lost silently.
    pub(super) fn close_all_tabs(&mut self) {
        while let Some(&id) = self.editor.workspace.tabs.last() {
            let idx = self.editor.workspace.tabs.len() - 1;
            let dirty = self
                .editor
                .workspace
                .documents
                .get(id)
                .map(|d| d.dirty)
                .unwrap_or(false);
            if dirty {
                self.request_close(idx); // prompt; re-run Close All after resolving it
                return;
            }
            self.remember_closed(idx);
            self.close_and_forget(idx);
        }
    }

    pub(super) fn cycle_tab(&mut self, delta: isize) {
        let n = self.editor.workspace.tabs.len();
        if n == 0 {
            return;
        }
        let cur = self.editor.workspace.active_tab as isize;
        let next = (cur + delta).rem_euclid(n as isize) as usize;
        self.editor.workspace.focus_tab(next);
    }

    /// Open a path in a tab, choosing *what kind of tab* by policy:
    ///
    /// 1. a directory re-roots the workspace (unchanged);
    /// 2. an already-open path just gets focused (unchanged);
    /// 3. a plugin viewer claiming the extension takes it (a `.pdf` → the `pdf` viewer);
    /// 4. otherwise [`files::open`] probes the header — binary or over the size ceiling opens a
    ///    *notice* tab explaining why, instead of spending four full passes over the file to
    ///    produce an uneditable wall of replacement characters;
    /// 5. only then is it read into a real buffer.
    ///
    /// Every route into a tab funnels here or through [`App::open_file_at_startup`], which
    /// applies the same policy — including session restore, so one accidental PDF can't poison
    /// every subsequent launch.
    pub(super) fn open_path(&mut self, path: &std::path::Path) {
        if path.is_dir() {
            self.editor.workspace.root = path.to_path_buf();
            return;
        }
        if let Some(id) = self.editor.workspace.find_by_path(path) {
            self.editor.workspace.focus_doc(id);
            return;
        }
        if let Some(viewer_id) = self.viewer_id_for(path) {
            self.open_viewer_tab(path, &viewer_id);
            return;
        }
        match files::open(path, &self.config.file_limits()) {
            Ok(files::Opened::Text(doc)) => self.open_text_tab(path, *doc),
            Ok(files::Opened::Refused(refusal)) => self.open_notice_tab(path, refusal),
            Err(e) => self.report_open_failure(path, &e),
        }
    }

    /// Report a failed open in the user's terms: what file, why, and — where one exists — the
    /// move that gets past it, named by its live chord.
    fn report_open_failure(&mut self, path: &std::path::Path, e: &anyhow::Error) {
        let mut msg = format!("Open failed: {}", io_reason(path, e));
        if let Some(recovery) = self.open_recovery(e) {
            msg.push_str(" — ");
            msg.push_str(&recovery);
        }
        self.editor.notify_error(msg);
    }

    /// The recovery that applies to an open failure, if any.
    pub(super) fn open_recovery(&self, e: &anyhow::Error) -> Option<String> {
        match io_kind(e)? {
            std::io::ErrorKind::NotFound => Some(format!(
                "{} opens a new empty buffer",
                self.chord_for("file.new", "Ctrl+N")
            )),
            std::io::ErrorKind::IsADirectory => Some(format!(
                "{} browses files instead",
                self.chord_for("view.quickOpen", "Ctrl+P")
            )),
            _ => None,
        }
    }

    /// The live chord bound to `id`, falling back to `default` when the user unbound it. Used to
    /// name a recovery in a message, so the text tracks the keymap instead of a hard-coded key.
    pub(super) fn chord_for(&self, id: &str, default: &str) -> String {
        self.keymap
            .binding_label(id)
            .unwrap_or_else(|| default.to_string())
    }

    /// The viewer id claiming `path`'s extension, if a loaded plugin contributes one. Disabling
    /// that plugin removes the claim, and the file falls back to the binary notice.
    pub(super) fn viewer_id_for(&self, path: &std::path::Path) -> Option<String> {
        let ext = path.extension()?.to_str()?;
        Some(self.registry.viewer_for_extension(ext)?.id.clone())
    }

    /// Put a loaded document in a tab and announce it.
    fn open_text_tab(&mut self, path: &std::path::Path, mut doc: Document) {
        doc.set_caret(0);
        let large = doc.large;
        let id = self.editor.workspace.open_document(doc);
        self.editor.emit(editor_plugin::event::Event::DidOpen(id));
        // Degraded mode is a persistent state, so it also gets a persistent `LARGE` segment in the
        // status bar; this says it once in words so the *reason* isn't left to be guessed.
        if large {
            self.editor.notify_warn(format!(
                "{} opened in large-file mode — syntax highlighting, git gutter, and LSP are off",
                display_name(path)
            ));
        }
        self.request_git_status(id);
    }

    /// Open the "lumina can't show this as text" tab for a refused file.
    fn open_notice_tab(&mut self, path: &std::path::Path, refusal: files::Refusal) {
        self.editor.open_notice_tab(path, refusal);
    }

    /// Open (or re-open) `path` in the viewer `viewer_id`, **replacing** any existing tab for
    /// that path — so `view.openAsHex` swaps a notice (or a clean text tab) in place rather than
    /// leaving two tabs claiming one file. Two documents with the same path would desynchronize:
    /// `find_by_path` answers with the first, so the watcher would reload one and leave the
    /// other stale.
    ///
    /// A *dirty* text tab is never closed for this — unsaved work outranks a view request.
    pub(super) fn open_viewer_tab(&mut self, path: &std::path::Path, viewer_id: &str) {
        let Some(title) = self.registry.viewer(viewer_id).map(|v| v.title.clone()) else {
            self.editor.notify_error(format!(
                "No viewer named “{viewer_id}” is loaded — the plugin providing it may be disabled \
                 in Settings → Plugins"
            ));
            return;
        };
        let abs = files::absolute_path(path);
        if let Some(existing) = self.editor.workspace.find_by_path(&abs) {
            let dirty = self
                .editor
                .workspace
                .documents
                .get(existing)
                .is_some_and(|d| d.dirty);
            if dirty {
                self.editor.workspace.focus_doc(existing);
                let save = self.chord_for("file.save", "Ctrl+S");
                self.editor.notify_warn(format!(
                    "{} has unsaved changes — {save} to save it first, then try again",
                    display_name(&abs)
                ));
                return;
            }
            if let Some(idx) = self
                .editor
                .workspace
                .tabs
                .iter()
                .position(|&t| t == existing)
            {
                self.close_and_forget(idx);
            }
        }
        let id = self.editor.open_viewer_tab(&abs, viewer_id, &title);
        self.render_viewer_tab(id);
    }

    /// `file.openAsText`: open the active view tab's file in a real text buffer, bypassing both
    /// a viewer's extension claim and the size ceiling.
    ///
    /// Without this a plugin claiming a *text* extension (`plugins/csvview` claims `.csv`) would
    /// make those files unopenable as text for as long as it is installed — a viewer could take
    /// a file hostage. Binary content is still refused: it cannot round-trip a UTF-8 rope.
    pub(super) fn open_as_text(&mut self) {
        let Some(path) = self
            .editor
            .active_tab_view()
            .and_then(|v| v.path())
            .map(|p| p.to_path_buf())
        else {
            return;
        };
        let limits = self.config.file_limits();
        match files::probe(&path) {
            Ok(probe) => {
                if let files::FileKind::Binary { label } = probe.kind {
                    self.editor.notify_warn(format!(
                        "{label} is not text — editing it here would corrupt the file"
                    ));
                    return;
                }
            }
            Err(e) => {
                self.report_open_failure(&path, &e);
                return;
            }
        }
        match files::open_forced(&path, &limits) {
            Ok(doc) => {
                let tab = self.editor.workspace.active_tab;
                self.close_and_forget(tab);
                self.open_text_tab(&path, doc);
            }
            Err(e) => self.report_open_failure(&path, &e),
        }
    }

    /// Ask the owning plugin to (re)render the viewer tab `id`. A viewer whose plugin has since
    /// been disabled leaves a stated reason rather than an empty pane.
    pub(super) fn render_viewer_tab(&mut self, id: editor_core::DocId) {
        let Some(crate::editor::TabView::Viewer(v)) = self.editor.tab_views.get(&id) else {
            return;
        };
        let (viewer_id, path) = (v.viewer_id.clone(), v.path.clone());
        if !self
            .registry
            .render_viewer(&viewer_id, id, &path, &mut self.editor)
        {
            if let Some(crate::editor::TabView::Viewer(v)) = self.editor.tab_views.get_mut(&id) {
                v.content = editor_plugin::ViewerContent::status_only(format!(
                    "The plugin providing the “{viewer_id}” viewer is not loaded."
                ));
            }
        }
        // Deliberately *not* draining effect queues here: this runs from inside the drain, and a
        // viewer that queued another `open_viewer` would recurse. Anything it queued lands on the
        // next tick, like every other plugin intent.
    }

    /// `file.openAnyway`: replace the active *notice* tab with a real text buffer, bypassing the
    /// size ceiling. Only offered for a size refusal — forcing binary content through the UTF-8
    /// rope would silently rewrite the file on the first save, so that refusal stands.
    pub(super) fn open_anyway(&mut self) {
        let Some(crate::editor::TabView::Notice { path, refusal }) = self.editor.active_tab_view()
        else {
            return;
        };
        let (path, refusal) = (path.clone(), *refusal);
        if !refusal.is_overridable() {
            self.editor.notify_warn(format!(
                "{} is not text — editing it here would corrupt the file",
                refusal.label()
            ));
            return;
        }
        match files::open_forced(&path, &self.config.file_limits()) {
            Ok(doc) => {
                let tab = self.editor.workspace.active_tab;
                self.close_and_forget(tab);
                self.open_text_tab(&path, doc);
            }
            Err(e) => self.report_open_failure(&path, &e),
        }
    }

    /// Save the active document, falling back to the Save As prompt when it has no path yet
    /// (plan §1.5 — resolves the old "Save As not yet wired" gap).
    pub(super) fn save_or_save_as(&mut self) {
        let has_path = self
            .editor
            .active_document()
            .map(|d| d.path.is_some())
            .unwrap_or(false);
        if has_path {
            self.save_active();
        } else {
            self.open_save_as();
        }
    }

    /// Open the Save As overlay, seeded with the current path (if any).
    ///
    /// Guarded here rather than in `save_or_save_as` because `file.saveAs` reaches this directly.
    /// `save_as_to` would repoint the placeholder at the typed path before `save_active` refused
    /// the write: no bytes lost, but the tab would then claim a file it isn't showing.
    pub(super) fn open_save_as(&mut self) {
        if let Some(view) = self.editor.active_tab_view() {
            let what = view
                .path()
                .map(display_name)
                .unwrap_or_else(|| "This tab".to_string());
            self.editor
                .notify_warn(format!("{what} is not a text buffer — nothing to save"));
            return;
        }
        if self.editor.active_document().is_none() {
            return;
        }
        let initial = self
            .editor
            .active_document()
            .and_then(|d| d.path.as_ref())
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.editor.overlay = Some(crate::editor::Overlay::SaveAsInput {
            buffer: initial,
            error: None,
            overwrite: None,
        });
    }

    /// Resolve what the user typed into the Save As box against the project root, exactly as
    /// [`Self::save_as_to`] will. Shared with the renderer so the box can *show* where the file
    /// will land before it lands there.
    pub(crate) fn resolve_save_as(&self, raw: &str) -> Option<PathBuf> {
        let raw = raw.trim();
        if raw.is_empty() {
            return None;
        }
        let path = PathBuf::from(raw);
        Some(if path.is_relative() {
            self.editor.workspace.root.join(path)
        } else {
            path
        })
    }

    /// Vet a Save As target before anything is written. Returns the message for the overlay's
    /// error slot when the path can't work — a missing parent directory or a directory in the
    /// file's place both used to surface only as `"Save failed: …"` after the fact.
    pub(super) fn save_as_problem(&self, path: &std::path::Path) -> Option<String> {
        if path.is_dir() {
            return Some(format!("{} is a directory", path.display()));
        }
        match path.parent() {
            Some(dir) if !dir.as_os_str().is_empty() && !dir.is_dir() => {
                Some(format!("No such directory: {}", dir.display()))
            }
            _ => None,
        }
    }

    /// Point the active document at `raw` (resolved against the project root when relative),
    /// refresh its language, and write it (plan §1.5).
    ///
    /// The overwrite check lives in the overlay (which can ask), not here: this is also the
    /// path a confirmed overwrite takes.
    pub(super) fn save_as_to(&mut self, raw: &str) {
        let Some(path) = self.resolve_save_as(raw) else {
            return;
        };
        let Some(id) = self.editor.workspace.active_doc() else {
            return;
        };
        if let Some(doc) = self.editor.workspace.documents.get_mut(id) {
            doc.path = Some(path.clone());
            doc.language = files::language_for(&path);
        }
        // Drop any stale highlighter so it re-creates for the (possibly new) language.
        self.editor.highlighters.remove(&id);
        self.save_active();
    }

    /// Open a fresh, empty, untitled buffer (plan §1.5).
    pub(super) fn new_file(&mut self) {
        let mut doc = Document::from_str("");
        doc.set_caret(0);
        self.editor.workspace.open_document(doc);
        self.editor.focus = Focus::Editor;
    }

    pub(super) fn save_active(&mut self) {
        // Read hygiene settings before borrowing the document (different `self` fields).
        let (trim, final_nl) = (
            self.config.trim_trailing_whitespace,
            self.config.insert_final_newline,
        );
        let Some(id) = self.editor.workspace.active_doc() else {
            return;
        };
        // The load-bearing guard: a notice/viewer tab's buffer is empty but its path points at a
        // real file, so saving it would replace that file with nothing.
        if let Some(view) = self.editor.tab_views.get(&id) {
            let what = view
                .path()
                .map(display_name)
                .unwrap_or_else(|| "This tab".to_string());
            self.editor
                .notify_warn(format!("{what} is not a text buffer — nothing to save"));
            return;
        }
        let save_as = self.chord_for("file.saveAs", "Ctrl+K Ctrl+S");
        let Some(doc) = self.editor.workspace.documents.get_mut(id) else {
            return;
        };
        let Some(path) = doc.path.clone() else {
            self.editor.notify_warn(format!(
                "This buffer has no file yet — {save_as} to name one"
            ));
            return;
        };
        // On-save hygiene runs as an undoable Transaction before the write (plan §1.4).
        if trim || final_nl {
            edit::apply_save_hygiene(doc, trim, final_nl);
        }
        match files::save(doc, &path) {
            Ok(fp) => {
                doc.dirty = false;
                doc.deleted_on_disk = false;
                // Record the hash we just wrote so the watch echo is suppressed (plan §6).
                self.pending_self_writes.insert(path.clone(), fp.hash);
                doc.disk = fp;
                doc.history.break_group();
                self.editor.notify_info(format!("Saved {}", path.display()));
                self.editor.emit(editor_plugin::event::Event::DidSave(id));
            }
            Err(e) => {
                // A failed save leaves the work only in the buffer, so the message has to carry
                // the way out, not just the diagnosis — and it is an Error, so it stays on screen
                // instead of vanishing under the next keystroke.
                self.editor.notify_error(format!(
                    "Save failed: {} — {save_as} to save it elsewhere",
                    io_reason(&path, &e)
                ));
            }
        }
        // Refresh the git gutter against the just-written file (plan §4.1).
        self.request_git_status(id);
    }
}

/// The file name of `path` for a user-facing message, falling back to the whole path.
pub(super) fn display_name(path: &std::path::Path) -> String {
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

/// The `io::ErrorKind` behind an `anyhow` chain, when there is one. File IO here is wrapped with
/// context (`"reading /path"`), so the kind has to be recovered rather than matched on directly.
pub(super) fn io_kind(err: &anyhow::Error) -> Option<std::io::ErrorKind> {
    err.chain()
        .find_map(|e| e.downcast_ref::<std::io::Error>())
        .map(|e| e.kind())
}

/// A plain-language sentence for a file operation that failed, always naming the file.
///
/// `io::Error`'s own text is written for whoever is reading a log — *"Permission denied (os error
/// 13)"* — and the anyhow context that wraps it names the operation, not the file the user thinks
/// in terms of. This maps the kinds a user can actually act on and keeps the raw text only as the
/// fallback for the ones they can't.
pub(super) fn io_reason(path: &std::path::Path, err: &anyhow::Error) -> String {
    use std::io::ErrorKind;
    let name = display_name(path);
    match io_kind(err) {
        Some(ErrorKind::NotFound) => format!("{name} doesn't exist"),
        Some(ErrorKind::PermissionDenied) => format!("no permission to write {name}"),
        Some(ErrorKind::IsADirectory) => format!("{name} is a directory, not a file"),
        Some(ErrorKind::NotADirectory) => {
            format!("part of the path to {name} is a file, not a directory")
        }
        Some(ErrorKind::ReadOnlyFilesystem) => format!("{name} is on a read-only filesystem"),
        Some(ErrorKind::StorageFull) => format!("no space left on the device holding {name}"),
        Some(ErrorKind::InvalidData) => format!("{name} isn't valid text"),
        // Anything else keeps the underlying text — vague is better than wrong — but still says
        // which file it was about, which the raw error often doesn't.
        _ => {
            let root = err
                .chain()
                .last()
                .map(|e| e.to_string())
                .unwrap_or_default();
            format!("{name}: {root}")
        }
    }
}
