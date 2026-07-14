//! Handling inbound LSP events: routing responses/notifications to plugins and applying edits.

use super::convert::{
    edit_is_stale, location_label, to_primitive_code_lens, to_primitive_completion,
    to_primitive_diag, to_primitive_inlay_hint, to_primitive_location, to_primitive_semantic_token,
    to_primitive_text_edit,
};
use super::*;

impl App {
    /// Act on a high-level LSP event (response or notification).
    pub(in crate::app) fn handle_lsp_event(&mut self, event: crate::lsp::LspEvent) {
        use crate::lsp::LspEvent;
        match event {
            LspEvent::Diagnostics(update) => {
                // Translate to primitive diagnostics and broadcast to the `diagnostics` plugin,
                // which owns the model (transport stays here).
                let doc = crate::lsp::path_from_uri(&update.uri)
                    .and_then(|path| self.editor.workspace.find_by_path(&path));
                let diagnostics = update
                    .diagnostics
                    .into_iter()
                    .map(to_primitive_diag)
                    .collect();
                self.editor
                    .pending_events
                    .push(editor_plugin::event::Event::LspDiagnostics { doc, diagnostics });
            }
            LspEvent::Hover(text) => {
                // Hand the rendered hover text to the `hover` plugin, which shows the info box.
                self.editor
                    .pending_events
                    .push(editor_plugin::event::Event::LspHover(text));
            }
            LspEvent::SignatureHelp(sig) => {
                // Hand the formatted signature (or None to clear) to the signature-help plugin.
                self.editor
                    .pending_events
                    .push(editor_plugin::event::Event::LspSignatureHelp(sig));
            }
            LspEvent::CodeActions(actions) => self.on_code_actions(actions),
            LspEvent::Highlights(hls) => {
                // Hand the occurrence ranges to the document-highlight plugin (it paints them).
                let hls = hls
                    .into_iter()
                    .map(|h| editor_plugin::LspHighlight {
                        line: h.line,
                        start_char16: h.start_char16,
                        end_line: h.end_line,
                        end_char16: h.end_char16,
                        kind: h.kind,
                    })
                    .collect();
                self.editor
                    .pending_events
                    .push(editor_plugin::event::Event::LspHighlights(hls));
            }
            LspEvent::Goto(loc) => {
                // Hand a single navigation target to the `lsp-nav` plugin, which jumps via
                // `Host::open_location` (the app owns the actual open + caret placement on drain).
                if let Some(location) = to_primitive_location(&loc) {
                    self.editor
                        .pending_events
                        .push(editor_plugin::event::Event::LspGoto(location));
                }
            }
            LspEvent::Completion(list) => {
                // Broadcast to the `completion` plugin, which anchors + filters into a popup.
                let items = list
                    .items
                    .into_iter()
                    .map(to_primitive_completion)
                    .collect();
                self.editor
                    .pending_events
                    .push(editor_plugin::event::Event::LspCompletion {
                        items,
                        is_incomplete: list.is_incomplete,
                    });
            }
            LspEvent::CompletionResolvedEdits { uri, edits } => {
                self.on_completion_resolved_edits(&uri, edits)
            }
            LspEvent::Rename(edit) => {
                // Resolve each file's URI to a path (version-checked) and hand the edit to the
                // `rename` plugin, which forwards it back through `Host::apply_workspace_edit`.
                let we = self.resolve_workspace_edit(edit);
                self.editor
                    .pending_events
                    .push(editor_plugin::event::Event::LspWorkspaceEdit(we));
            }
            LspEvent::Message(text) => {
                self.editor.status_message = Some(format!("LSP: {text}"));
            }
            LspEvent::Progress(line) => {
                // Store the rendered work-done progress as a statusline item (spinner added at
                // render); an empty update clears it (§1.5).
                match line {
                    Some(line) => self.editor.status_items.insert("lsp.progress".into(), line),
                    None => self.editor.status_items.remove("lsp.progress"),
                };
            }
            LspEvent::SemanticTokens { uri, tokens } => {
                // Resolve to the doc it was computed for (not whatever is active now) and broadcast
                // to the `semantic-tokens` plugin, which paints them over tree-sitter (§7.1).
                let doc = crate::lsp::path_from_uri(&uri)
                    .and_then(|path| self.editor.workspace.find_by_path(&path));
                let tokens = tokens
                    .into_iter()
                    .map(to_primitive_semantic_token)
                    .collect();
                self.editor
                    .pending_events
                    .push(editor_plugin::event::Event::LspSemanticTokens { doc, tokens });
            }
            LspEvent::SemanticTokensRefresh { lang } => self.resync_semantic_tokens(&lang),
            LspEvent::InlayHints { uri, hints } => {
                let doc = crate::lsp::path_from_uri(&uri)
                    .and_then(|path| self.editor.workspace.find_by_path(&path));
                let hints = hints.into_iter().map(to_primitive_inlay_hint).collect();
                self.editor
                    .pending_events
                    .push(editor_plugin::event::Event::LspInlayHints { doc, hints });
            }
            LspEvent::InlayHintRefresh { lang } => self.resync_inlay_hints(&lang),
            LspEvent::CodeLenses { uri, lenses } => {
                let doc = crate::lsp::path_from_uri(&uri)
                    .and_then(|path| self.editor.workspace.find_by_path(&path));
                let lenses = lenses.into_iter().map(to_primitive_code_lens).collect();
                self.editor
                    .pending_events
                    .push(editor_plugin::event::Event::LspCodeLenses { doc, lenses });
            }
            LspEvent::FoldingRanges { uri, ranges } => {
                let doc = crate::lsp::path_from_uri(&uri)
                    .and_then(|path| self.editor.workspace.find_by_path(&path));
                let ranges = ranges
                    .into_iter()
                    .map(|r| editor_plugin::LspFoldingRange {
                        start_line: r.start_line,
                        end_line: r.end_line,
                        kind: r.kind,
                    })
                    .collect();
                self.editor
                    .pending_events
                    .push(editor_plugin::event::Event::LspFoldingRanges { doc, ranges });
            }
            LspEvent::CodeLensRefresh { lang } => self.resync_code_lens(&lang),
            LspEvent::ServerExited { lang } => self.forget_synced_docs(&lang),
            LspEvent::DiagnosticsRefresh { lang } => self.rearm_pull(&lang),
            LspEvent::ServerRequest {
                lang,
                id,
                method,
                params,
            } => self.handle_server_request(lang, id, &method, params),
            LspEvent::References(locs) => {
                let items = locs
                    .iter()
                    .filter_map(|l| {
                        Some(editor_plugin::LspNavItem {
                            location: to_primitive_location(l)?,
                            label: location_label(l),
                        })
                    })
                    .collect();
                self.push_locations("References", items);
            }
            LspEvent::DocumentSymbols(syms) => {
                // Every symbol is in the active document; resolve its path once.
                let Some(path) = self
                    .editor
                    .active_document()
                    .and_then(|d| d.path.clone())
                    .map(|p| p.to_string_lossy().into_owned())
                else {
                    return;
                };
                let items = syms
                    .into_iter()
                    .map(|s| editor_plugin::LspNavItem {
                        label: format!("{}{}", "  ".repeat(s.depth), s.name),
                        location: editor_plugin::LspLocation {
                            path: path.clone(),
                            line: s.line,
                            character: s.character,
                        },
                    })
                    .collect();
                self.push_locations("Symbols", items);
            }
            LspEvent::WorkspaceSymbols(syms) => {
                let items = syms
                    .iter()
                    .filter_map(|(name, loc)| {
                        let location = to_primitive_location(loc)?;
                        Some(editor_plugin::LspNavItem {
                            label: format!("{name}  {}", location_label(loc)),
                            location,
                        })
                    })
                    .collect();
                self.push_locations("Workspace Symbols", items);
            }
            LspEvent::Formatting { uri, edits } => {
                // Apply formatting to the document it was requested against (resolved from the
                // response's uri, NOT the currently-active doc — a tab switch during the async
                // round-trip must not misapply the edits elsewhere). One atomic group, inv #1.
                let Some(path) = crate::lsp::path_from_uri(&uri) else {
                    return;
                };
                let edits = edits.into_iter().map(to_primitive_text_edit).collect();
                let edit = editor_plugin::LspWorkspaceEdit {
                    changes: vec![(path.to_string_lossy().into_owned(), edits)],
                };
                if self.apply_workspace_edit(edit) > 0 {
                    self.editor.status_message = Some("Formatted document".to_string());
                }
            }
            LspEvent::Error(message) => {
                self.editor.status_message = Some(format!("LSP: {message}"));
            }
        }
    }

    /// Convert code actions (URIs → paths, version-checked) and hand the list to the code-action
    /// plugin, which shows a picker and applies the chosen one.
    fn on_code_actions(&mut self, actions: Vec<editor_lsp::CodeAction>) {
        let prim: Vec<_> = actions
            .into_iter()
            .map(|a| editor_plugin::LspCodeAction {
                title: a.title,
                edit: a
                    .edit
                    .map(|e| self.resolve_workspace_edit(e))
                    .unwrap_or_default(),
                command: a.command.map(|c| (c.command, c.arguments)),
            })
            .collect();
        self.editor
            .pending_events
            .push(editor_plugin::event::Event::LspCodeActions(prim));
    }

    /// Late auto-import edits from `completionItem/resolve` → apply to the doc they resolved for.
    fn on_completion_resolved_edits(&mut self, uri: &str, edits: Vec<editor_lsp::TextEdit>) {
        if edits.is_empty() {
            return;
        }
        let Some(path) = crate::lsp::path_from_uri(uri) else {
            return;
        };
        let edits = edits.into_iter().map(to_primitive_text_edit).collect();
        self.apply_workspace_edit(editor_plugin::LspWorkspaceEdit {
            changes: vec![(path.to_string_lossy().into_owned(), edits)],
        });
    }

    /// The `(path, language)` of every open doc of `lang` — the fan-out set for a server refresh.
    fn docs_of_lang(&self, lang: &str) -> Vec<(PathBuf, String)> {
        self.editor
            .workspace
            .documents
            .iter()
            .filter(|(_, d)| d.language.as_deref() == Some(lang))
            .filter_map(|(_, d)| Some((d.path.clone()?, d.language.clone()?)))
            .collect()
    }

    /// The [`DocId`](editor_core::DocId)s of every open doc of `lang`.
    fn doc_ids_of_lang(&self, lang: &str) -> Vec<editor_core::DocId> {
        self.editor
            .workspace
            .documents
            .iter()
            .filter(|(_, d)| d.language.as_deref() == Some(lang))
            .map(|(id, _)| id)
            .collect()
    }

    /// Re-request semantic tokens for every open doc of `lang` (§7.1 refresh).
    fn resync_semantic_tokens(&mut self, lang: &str) {
        for (p, l) in self.docs_of_lang(lang) {
            self.lsp.request_semantic_tokens(&p, &l);
        }
    }

    /// Re-request inlay hints for every open doc of `lang` (§7.2 refresh).
    fn resync_inlay_hints(&mut self, lang: &str) {
        for (p, l) in self.docs_of_lang(lang) {
            let end_line = self
                .editor
                .workspace
                .find_by_path(&p)
                .and_then(|id| self.editor.workspace.documents.get(id))
                .map(|d| d.len_lines() as u32)
                .unwrap_or(0);
            self.lsp.request_inlay_hints(&p, &l, end_line);
        }
    }

    /// Re-request code lenses for every open doc of `lang` (§6.4 refresh).
    fn resync_code_lens(&mut self, lang: &str) {
        for (p, l) in self.docs_of_lang(lang) {
            self.lsp.request_code_lens(&p, &l);
        }
    }

    /// A server for `lang` exited: forget the per-doc sync + pull bookkeeping so `update_lsp`
    /// re-sends `didOpen` and re-pulls after the restart (§3.9). Versions stay monotonic.
    fn forget_synced_docs(&mut self, lang: &str) {
        for id in self.doc_ids_of_lang(lang) {
            self.lsp_sent_revision.remove(&id);
            self.lsp_pulled_revision.remove(&id);
            self.lsp_pull_deadline.remove(&id);
        }
    }

    /// Re-arm the debounced diagnostics pull for `lang`'s open docs (§5.1 `diagnostic/refresh`).
    fn rearm_pull(&mut self, lang: &str) {
        for id in self.doc_ids_of_lang(lang) {
            self.lsp_pulled_revision.remove(&id);
            self.lsp_pull_deadline.remove(&id);
        }
    }

    /// Resolve an `editor-lsp` `WorkspaceEdit`'s URIs to filesystem paths and convert to the kernel
    /// primitive, **dropping any file whose server-declared version no longer matches the buffer**
    /// (§2.4 staleness — applying stale edits at drifted offsets would corrupt the file). Shared by
    /// rename, code actions, and server-initiated `workspace/applyEdit`; non-`file:` URIs dropped.
    pub(super) fn resolve_workspace_edit(
        &mut self,
        edit: editor_lsp::WorkspaceEdit,
    ) -> editor_plugin::LspWorkspaceEdit {
        let mut stale = 0usize;
        let changes = edit
            .changes
            .into_iter()
            .filter_map(|d| {
                if edit_is_stale(d.version, self.lsp.doc_version(&d.uri)) {
                    stale += 1;
                    return None;
                }
                let path = crate::lsp::path_from_uri(&d.uri)?;
                let edits = d.edits.into_iter().map(to_primitive_text_edit).collect();
                Some((path.to_string_lossy().into_owned(), edits))
            })
            .collect();
        if stale > 0 {
            self.editor.status_message = Some(format!("Skipped {stale} stale edit(s)"));
        }
        editor_plugin::LspWorkspaceEdit { changes }
    }

    /// Hand a navigation location set (references / document symbols) to the `lsp-nav` plugin,
    /// which opens the picker and owns the jump. Shared by both response arms.
    fn push_locations(&mut self, title: &str, items: Vec<editor_plugin::LspNavItem>) {
        self.editor
            .pending_events
            .push(editor_plugin::event::Event::LspLocations {
                title: title.to_string(),
                items,
            });
    }

    /// Apply a rename's edits across the affected documents as history-recorded transactions. The
    /// `rename` plugin forwards a primitive [`editor_plugin::LspWorkspaceEdit`] (paths already
    /// resolved) here through the effect-queue; the app owns the file IO + UTF-16↔char mapping.
    /// Returns the number of files actually changed (0 = nothing applied), so a server
    /// `workspace/applyEdit` can report `applied` honestly.
    pub(in crate::app) fn apply_workspace_edit(
        &mut self,
        edit: editor_plugin::LspWorkspaceEdit,
    ) -> usize {
        let mut count = 0usize;
        for (path, edits) in edit.changes {
            let path = std::path::PathBuf::from(path);
            if self.editor.workspace.find_by_path(&path).is_none() {
                self.open_path(&path);
            }
            let Some(id) = self.editor.workspace.find_by_path(&path) else {
                continue;
            };
            let Some(doc) = self.editor.workspace.documents.get_mut(id) else {
                continue;
            };
            let mut changes: Vec<editor_core::transaction::Change> = edits
                .iter()
                .map(|te| {
                    let start = lsp_pos_to_char(doc, te.start_line, te.start_char16);
                    let end = lsp_pos_to_char(doc, te.end_line, te.end_char16);
                    let (lo, hi) = (start.min(end), start.max(end));
                    editor_core::transaction::Change {
                        at: lo,
                        removed: doc.rope().slice(lo..hi).to_string(),
                        inserted: te.new_text.clone(),
                    }
                })
                .collect();
            changes.sort_by_key(|c| c.at);
            let txn = editor_core::Transaction::from_changes(changes);
            if txn.is_empty() {
                continue;
            }
            let before = doc.selections.clone();
            let inverse = txn.apply(doc);
            doc.dirty = true;
            let after = doc.selections.clone();
            doc.history
                .record(txn, inverse, before, after, editor_core::GroupBreak::Force);
            count += 1;
        }
        if count > 0 {
            self.editor.status_message = Some(format!("Applied edits across {count} file(s)"));
        }
        count
    }
}
