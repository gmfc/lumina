//! Draining background-worker messages and reacting to external disk changes.
//!
//! Part of the [`crate::app`] module; these are `impl App` blocks split out by concern.

use super::*;

impl App {
    pub(super) fn drain_workers(&mut self) {
        // Apply any queued opens/commands/events produced during dispatch.
        let opens: Vec<PathBuf> = std::mem::take(&mut self.editor.pending_opens);
        for path in opens {
            self.open_path(&path);
        }
        let cmds: Vec<String> = std::mem::take(&mut self.editor.pending_commands);
        for id in cmds {
            self.registry.dispatch_command(&id, &mut self.editor);
        }
        // Coalesce duplicate notifications from this tick: a burst of external changes (e.g. a
        // build touching files, or a formatter rewriting several open tabs) can enqueue the same
        // idempotent event many times, and each broadcast makes reactive plugins (like the
        // explorer's full re-walk) redo the same work. Keep first occurrences, preserving order.
        let mut events = std::mem::take(&mut self.editor.pending_events);
        let mut seen = Vec::new();
        events.retain(|ev| {
            if seen.contains(ev) {
                false
            } else {
                seen.push(ev.clone());
                true
            }
        });
        for ev in events {
            self.registry.broadcast(&ev, &mut self.editor);
        }

        // LSP responses/notifications: diagnostics, hover, goto, completion, rename.
        for event in self.lsp.poll() {
            self.handle_lsp_event(event);
        }

        self.drain_worker_channel();
    }

    /// Drain background worker messages (FS watch, git, project search) into state.
    pub(super) fn drain_worker_channel(&mut self) {
        use crate::worker::WorkerMsg;
        // Cap terminal bytes processed per tick so a flooding shell (e.g. `yes`) can't starve
        // the render/input loop — the UI stays responsive, so Ctrl+C (which stops the flood)
        // remains reachable. Anything past the budget stays queued for the next ticks.
        const TERM_BYTE_BUDGET: usize = 1 << 20; // 1 MiB
        let mut term_bytes = 0usize;
        while let Ok(msg) = self.worker_rx.try_recv() {
            match msg {
                WorkerMsg::DiskChanged { path } => self.on_disk_changed(&path),
                WorkerMsg::GitStatus { path, statuses } => {
                    if let Some(id) = self.editor.workspace.find_by_path(&path) {
                        self.editor.git_hunks.insert(id, statuses);
                    }
                }
                WorkerMsg::SearchComplete { query, hits } => self.on_search_complete(query, hits),
                WorkerMsg::TerminalOutput { id, bytes } => {
                    term_bytes += bytes.len();
                    if let Some(t) = self.panel.terminal_mut(id) {
                        t.feed(&bytes);
                    }
                    if term_bytes >= TERM_BYTE_BUDGET {
                        break;
                    }
                }
                WorkerMsg::TerminalExited { id } => {
                    if let Some(t) = self.panel.terminal_mut(id) {
                        t.mark_exited();
                    }
                }
            }
        }
    }

    /// Fold a completed project search into the open search panel, if it's still the live query.
    pub(super) fn on_search_complete(
        &mut self,
        query: String,
        hits: Vec<crate::search::SearchHit>,
    ) {
        if let Some(search) = &mut self.search {
            if search.query == query {
                search.results = hits;
                search.selected = 0;
                search.running = false;
            }
        }
    }

    /// Reconcile an external on-disk change against the buffer (plan §6 decision matrix).
    pub(super) fn on_disk_changed(&mut self, path: &std::path::Path) {
        // A change to the user config file → hot-reload keymap/settings (plan §6).
        if self.config_path.as_deref() == Some(path) {
            self.reload_config();
            return;
        }

        // Not one of our open docs → refresh the tree and move on.
        let Some(id) = self.editor.workspace.find_by_path(path) else {
            self.editor
                .pending_events
                .push(editor_plugin::event::Event::DidChangeConfig);
            // Also nudge the explorer to rescan on any tree change.
            return;
        };

        let Ok(bytes) = std::fs::read(path) else {
            // Deleted mid-race, or unreadable → flag deletion, keep the buffer.
            if let Some(doc) = self.editor.workspace.documents.get_mut(id) {
                doc.deleted_on_disk = true;
                doc.dirty = true; // a save re-creates it
            }
            return;
        };
        let fp = crate::files::fingerprint(&bytes);

        // Our own save echoing back → drop it.
        if self.pending_self_writes.get(path) == Some(&fp.hash) {
            self.pending_self_writes.remove(path);
            if let Some(doc) = self.editor.workspace.documents.get_mut(id) {
                doc.disk = fp;
            }
            return;
        }

        let Some(doc) = self.editor.workspace.documents.get_mut(id) else {
            return;
        };
        // No real change (hash matches last-loaded) → just refresh the fingerprint.
        if doc.disk.hash == fp.hash {
            doc.disk = fp;
            return;
        }

        if doc.dirty {
            // Never clobber unsaved work — flag a conflict for the user to resolve.
            doc.external_conflict = Some(fp);
            return;
        }

        // Clean buffer → reload, following the cursor/scroll through the diff.
        let new_text = String::from_utf8_lossy(&bytes).into_owned();
        let old_text = doc.to_string();
        let heads: Vec<usize> = doc.selections.ranges().iter().map(|s| s.head).collect();
        let mapped: Vec<usize> = heads
            .iter()
            .map(|&h| crate::sync::map_offset(&old_text, &new_text, h))
            .collect();

        doc.set_text_str(&new_text);
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
            doc.view.scroll_line = line.saturating_sub(2);
        }
        self.editor
            .pending_events
            .push(editor_plugin::event::Event::ExternalReload(id));
        // Find matches held raw offsets into the *old* text; recompute them against the reload
        // so a later replace can't slice past the new buffer end (which would panic).
        self.refresh_find_after_reload(id);
        // The file changed under us (e.g. an agent wrote it) — refresh its git gutter.
        self.request_git_status(id);
    }
}
