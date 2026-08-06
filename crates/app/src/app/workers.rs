//! Draining background-worker messages and reacting to external disk changes.
//!
//! Part of the [`crate::app`] module; these are `impl App` blocks split out by concern.

use super::*;

impl App {
    /// Returns whether anything was actually processed this tick — the idle-frame gate repaints
    /// only when it did (a false return means no async work, so the frame can be skipped).
    pub(super) fn drain_workers(&mut self) -> bool {
        // Emit `DidChangeActive` when the focused document changed since last tick, so reactive
        // plugins clear per-doc UI on a tab switch (completion popup, signature/highlight, the
        // diagnostics status). Nothing else emits this event.
        let active = self.editor.workspace.active_doc();
        let active_changed = active != self.last_active;
        if active_changed {
            self.last_active = active;
            self.editor
                .pending_events
                .push(editor_plugin::event::Event::DidChangeActive(active));
        }
        // Apply a queued appearance toggle (the theme is app-owned render state).
        let theme_toggled = std::mem::take(&mut self.editor.pending_theme_toggle);
        if theme_toggled {
            self.toggle_theme();
        }
        // Any non-empty intent queue (captured before draining, since draining may repopulate them
        // for the *next* tick) means a plugin queued visible work this tick.
        let had_pending = active_changed
            || !self.editor.pending_lsp_requests.is_empty()
            || !self.editor.pending_opens.is_empty()
            || !self.editor.pending_viewers.is_empty()
            || !self.editor.pending_commands.is_empty()
            || !self.editor.pending_events.is_empty()
            || !self.editor.pending_locations.is_empty()
            || !self.editor.pending_workspace_edits.is_empty();
        self.drain_pending_lsp_requests();
        self.drain_pending_opens();
        self.drain_pending_viewers();
        self.drain_pending_commands();
        self.broadcast_pending_events();
        // The following are drained *after* the broadcast so intents a plugin queued while reacting
        // to an event (a nav jump, a rename edit) land in the same pass.
        self.drain_pending_locations();
        self.drain_pending_workspace_edits();
        // LSP responses/notifications: diagnostics, hover, goto, completion, rename.
        let mut any_lsp = false;
        for event in self.lsp.poll() {
            any_lsp = true;
            self.handle_lsp_event(event);
        }
        let any_msg = self.drain_worker_channel();
        // An open notification tab tracks the log rather than snapshotting it at open time.
        self.refresh_notification_log();
        had_pending || theme_toggled || any_lsp || any_msg
    }

    /// Forward queued LSP requests to the (app-owned) manager, resolving the cursor position
    /// app-side (the plugin only expressed intent).
    fn drain_pending_lsp_requests(&mut self) {
        for kind in std::mem::take(&mut self.editor.pending_lsp_requests) {
            self.dispatch_lsp_request(kind);
        }
    }

    /// Apply queued file opens (each optionally jumping to a line).
    fn drain_pending_opens(&mut self) {
        let opens: Vec<(PathBuf, Option<usize>)> = std::mem::take(&mut self.editor.pending_opens);
        for (path, line) in opens {
            self.open_path(&path);
            if let Some(line) = line {
                self.goto_line(line);
            }
        }
    }

    /// Apply viewer opens queued via `Host::open_viewer` (a plugin only expresses intent; the app
    /// owns file IO policy and the tab).
    fn drain_pending_viewers(&mut self) {
        let viewers: Vec<(PathBuf, String)> = std::mem::take(&mut self.editor.pending_viewers);
        for (path, viewer_id) in viewers {
            self.open_viewer_tab(&path, &viewer_id);
        }
    }

    /// Run queued command ids through the full `exec_id` precedence — a plugin can queue *any* id
    /// via `Host::execute` (the palette does this for the selected row), including the app-level
    /// stringly ids (`view.settings`, `config.reload`), not just registry commands.
    fn drain_pending_commands(&mut self) {
        for id in std::mem::take(&mut self.editor.pending_commands) {
            self.exec_id(&id);
        }
    }

    /// Broadcast queued plugin events, coalescing duplicates from this tick: a burst of external
    /// changes can enqueue the same idempotent event many times, and each broadcast makes reactive
    /// plugins redo the same work. Keeps first occurrences, preserving order.
    fn broadcast_pending_events(&mut self) {
        let events = std::mem::take(&mut self.editor.pending_events);
        // Skip an index iff an equal event appeared earlier this tick (first occurrence wins, order
        // preserved). Compared by value against the already-seen prefix — no `clone` of the event's
        // payload (some events, e.g. `JobComplete`, carry a heap buffer). `n` is a handful per tick,
        // so the O(n²) compare is cheaper than cloning every unique event into a scratch set.
        for i in 0..events.len() {
            if events[..i].contains(&events[i]) {
                continue;
            }
            self.registry.broadcast(&events[i], &mut self.editor);
        }
    }

    /// Apply LSP navigation jumps queued via `Host::open_location` (the `lsp-nav` plugin only
    /// expresses intent): open the target and resolve its UTF-16 column to a char offset now that
    /// the doc is loaded (app owns IO).
    fn drain_pending_locations(&mut self) {
        let locations: Vec<(PathBuf, u32, u32)> =
            std::mem::take(&mut self.editor.pending_locations);
        for (path, line, character) in locations {
            self.open_path(&path);
            if let Some(doc) = self.editor.active_document_mut() {
                let off = crate::app::lsp_pos_to_char(doc, line, character);
                doc.set_caret(off);
            }
        }
    }

    /// Apply multi-file rename/code-action edits queued via `Host::apply_workspace_edit` (the
    /// `rename` plugin only forwards the edit); the app opens each file and applies the edits.
    fn drain_pending_workspace_edits(&mut self) {
        let workspace_edits: Vec<editor_plugin::LspWorkspaceEdit> =
            std::mem::take(&mut self.editor.pending_workspace_edits);
        for edit in workspace_edits {
            self.apply_workspace_edit(edit);
        }
    }

    /// Drain background worker messages (FS watch, git, project search) into state. Returns whether
    /// at least one message was processed (feeds the idle-frame gate).
    pub(super) fn drain_worker_channel(&mut self) -> bool {
        use crate::worker::WorkerMsg;
        // Cap terminal bytes processed per tick so a flooding shell (e.g. `yes`) can't starve
        // the render/input loop — the UI stays responsive, so Ctrl+C (which stops the flood)
        // remains reachable. Anything past the budget stays queued for the next ticks.
        const TERM_BYTE_BUDGET: usize = 1 << 20; // 1 MiB
        let mut term_bytes = 0usize;
        let mut processed = false;
        while let Ok(msg) = self.worker_rx.try_recv() {
            processed = true;
            match msg {
                WorkerMsg::DiskChanged { path } => self.on_disk_changed(&path),
                WorkerMsg::GitStatus { path, statuses } => {
                    if let Some(id) = self.editor.workspace.find_by_path(&path) {
                        self.editor.git_hunks.insert(id, statuses);
                    }
                }
                WorkerMsg::JobComplete { id, payload } => {
                    // Fold plugin job results back into single-threaded dispatch: broadcast as an
                    // event the owning plugin decodes in `on_event`.
                    self.editor
                        .pending_events
                        .push(editor_plugin::event::Event::JobComplete { id, payload });
                }
                WorkerMsg::TerminalOutput { id, bytes } => {
                    term_bytes += bytes.len();
                    if let Some(t) = self
                        .editor
                        .terminals
                        .get_mut(&editor_plugin::TerminalId(id))
                    {
                        t.feed(&bytes);
                    }
                    if term_bytes >= TERM_BYTE_BUDGET {
                        break;
                    }
                }
                WorkerMsg::TerminalExited { id } => {
                    if let Some(t) = self
                        .editor
                        .terminals
                        .get_mut(&editor_plugin::TerminalId(id))
                    {
                        t.mark_exited();
                    }
                }
            }
        }
        processed
    }

    /// Reconcile an external on-disk change against the buffer (plan §6 decision matrix).
    pub(super) fn on_disk_changed(&mut self, path: &std::path::Path) {
        // Forward the tree change to any language server that dynamically registered a matching
        // file watcher (§8.1) — independent of whether it's an open document or the config file.
        self.lsp.notify_watched_file_change(path);

        // A change to either config file — global or project-local — hot-reloads keymap/settings
        // (plan §6). The project one sits inside the watched tree, so it arrives here too.
        if self.config_path.as_deref() == Some(path) || self.project_config_path == path {
            self.reload_config();
            return;
        }

        // A viewer tab showing this file re-renders from the new bytes; a notice tab re-probes,
        // so shrinking a file below the ceiling turns its notice into a real buffer on reopen.
        // Neither may take the text-reload path below — their buffer is an empty placeholder,
        // and "reloading" it would replace the tab's contents with the file's raw bytes.
        if let Some(id) = self.editor.workspace.find_by_path(path) {
            if self.editor.is_tab_view(id) {
                self.render_viewer_tab(id);
                return;
            }
        }

        // Not one of our open docs → refresh the tree and move on.
        let Some(id) = self.editor.workspace.find_by_path(path) else {
            self.editor
                .pending_events
                .push(editor_plugin::event::Event::DidChangeConfig);
            // Also nudge the explorer to rescan on any tree change.
            return;
        };

        let limits = self.config.file_limits();
        if let Some(refusal) = reload_refusal(path, &limits) {
            self.editor.notify_warn(refusal);
            return;
        }
        let Ok(bytes) = std::fs::read(path) else {
            // Deleted mid-race, or unreadable → flag deletion, keep the buffer.
            if let Some(doc) = self.editor.workspace.documents.get_mut(id) {
                doc.deleted_on_disk = true;
                doc.dirty = true; // a save re-creates it
            }
            return;
        };
        // A file that grew across the degraded-mode threshold while open must pick that up, or
        // it keeps its highlighter and language server while being reloaded at any size.
        if let Some(doc) = self.editor.workspace.documents.get_mut(id) {
            doc.large = limits.is_large(bytes.len() as u64);
            if doc.large {
                self.editor.highlighters.remove(&id);
            }
        }
        let fp = crate::files::fingerprint(&bytes);

        match self.classify_change(id, path, &fp) {
            Change::Ignore => {}
            Change::Conflict => self.flag_conflict(id, path, fp),
            Change::Reload => self.reload_clean(id, path, &bytes, fp),
        }
    }

    /// Decide what an external change to an *open, readable* document means. Splitting the
    /// decision from the acting on it is what keeps each side legible: this arm reads state and
    /// commits nothing, and the three outcomes below each do one thing.
    ///
    /// `Ignore` covers the two no-ops that still update bookkeeping: our own save echoing back
    /// through the watcher, and a change whose content matches what we last loaded.
    fn classify_change(
        &mut self,
        id: editor_core::DocId,
        path: &std::path::Path,
        fp: &editor_core::document::DiskFingerprint,
    ) -> Change {
        // Our own save echoing back → drop it, but adopt the fingerprint.
        if self.pending_self_writes.get(path) == Some(&fp.hash) {
            self.pending_self_writes.remove(path);
            if let Some(doc) = self.editor.workspace.documents.get_mut(id) {
                doc.disk = fp.clone();
            }
            return Change::Ignore;
        }
        let Some(doc) = self.editor.workspace.documents.get_mut(id) else {
            return Change::Ignore;
        };
        // No real change (hash matches last-loaded) → just refresh the fingerprint.
        if doc.disk.hash == fp.hash {
            doc.disk = fp.clone();
            return Change::Ignore;
        }
        if doc.dirty {
            Change::Conflict
        } else {
            Change::Reload
        }
    }

    /// Never clobber unsaved work: record the conflict and, on the transition into it, say what
    /// happened and name both exits.
    ///
    /// A `⚠` sharing the tab bar's marker slot is the whole announcement otherwise. Only on the
    /// transition — a busy writer re-fires this every debounce tick.
    fn flag_conflict(
        &mut self,
        id: editor_core::DocId,
        path: &std::path::Path,
        fp: editor_core::document::DiskFingerprint,
    ) {
        let Some(doc) = self.editor.workspace.documents.get_mut(id) else {
            return;
        };
        let first = doc.external_conflict.is_none();
        doc.external_conflict = Some(fp);
        if !first {
            return;
        }
        let name = super::file_ops::display_name(path);
        let palette = self.chord_for("view.commandPalette", "Ctrl+Shift+P");
        self.editor.notify_warn(format!(
            "{name} changed on disk and your buffer has unsaved changes — \
             {palette}: “Revert File” takes the disk version, “Keep My Version” keeps yours"
        ));
    }

    /// Clean buffer → reload, following the cursor/scroll through the diff.
    ///
    /// Decodes with the same encoding-aware path as the initial load (strip/track a BOM, decode
    /// UTF-16), not a bare `from_utf8_lossy` — otherwise a UTF-16 or BOM file reloads as mojibake
    /// and the stale `doc.encoding` re-encodes that garbage on the next save (and a BOM file would
    /// accrue a second BOM each cycle).
    fn reload_clean(
        &mut self,
        id: editor_core::DocId,
        path: &std::path::Path,
        bytes: &[u8],
        fp: editor_core::document::DiskFingerprint,
    ) {
        let (new_text, encoding) = crate::files::decode(bytes);
        let Some(doc) = self.editor.workspace.documents.get_mut(id) else {
            return;
        };
        let old_text = doc.to_string();
        let mapped: Vec<usize> = doc
            .selections
            .ranges()
            .iter()
            .map(|s| crate::sync::map_offset(&old_text, &new_text, s.head))
            .collect();

        // Whole-buffer reload: replaces the text and discards the now-stale undo history (its
        // transactions were recorded against the old offsets — see Document::reload_from_str).
        doc.reload_from_str(&new_text);
        doc.encoding = encoding;
        let clamped: Vec<editor_core::Selection> = mapped
            .iter()
            .map(|&m| editor_core::Selection::caret(doc.clamp(m)))
            .collect();
        doc.selections = editor_core::Selections::from_iter(clamped);
        doc.disk = fp;
        doc.dirty = false;
        doc.externally_reloaded = true;

        if self.follow_mode {
            let line = crate::sync::first_changed_line(&old_text, &new_text);
            if let Some(doc) = self.editor.workspace.documents.get_mut(id) {
                doc.view.scroll_line = line.saturating_sub(2);
            }
        }
        // `reload_from_str` drops undo history along with the text (its transactions reference
        // offsets that no longer exist). That is correct and invisible — the next Ctrl+Z simply
        // does nothing — so it has to be said out loud.
        self.editor.notify_info(format!(
            "{} reloaded from disk — undo history for this file was reset",
            super::file_ops::display_name(path)
        ));
        self.editor
            .pending_events
            .push(editor_plugin::event::Event::ExternalReload(id));
        // The `find` plugin re-derives its matches against the reload on this ExternalReload event
        // (its `on_event`), so a later replace can't slice past the new buffer end.
        // The file changed under us (e.g. an agent wrote it) — refresh its git gutter.
        self.request_git_status(id);
    }
}

/// What an external change to an open, readable document turns out to mean.
enum Change {
    /// Nothing to do — an echo of our own save, or content identical to what we loaded.
    Ignore,
    /// The buffer has unsaved changes; the disk copy must not overwrite them.
    Conflict,
    /// The buffer is clean; take the disk copy.
    Reload,
}

/// Why a changed file must *not* be reloaded into its open buffer, or `None` when it may be.
///
/// Re-applies the open policy before reading: an external process can turn a 4 KB log into a 3 GB
/// one (or a text file into binary) while it is open, and an unguarded reload would slurp all of
/// it on the UI thread — the exact freeze `files::open` prevents at open time, reached through the
/// back door. An unreadable file returns `None` and falls through to the caller's deletion
/// handling.
fn reload_refusal(path: &std::path::Path, limits: &crate::files::Limits) -> Option<String> {
    let probe = crate::files::probe(path).ok()?;
    if let crate::files::FileKind::Binary { label } = probe.kind {
        return Some(format!(
            "{} changed on disk and is no longer text ({label}) — not reloaded",
            super::file_ops::display_name(path)
        ));
    }
    if limits.max_bytes > 0 && probe.len > limits.max_bytes {
        return Some(format!(
            "{} grew past the {} open limit — not reloaded",
            super::file_ops::display_name(path),
            crate::files::human_bytes(limits.max_bytes)
        ));
    }
    None
}
