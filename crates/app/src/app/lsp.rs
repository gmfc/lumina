//! LSP glue: notifying the server, issuing requests, and handling responses.
//!
//! Part of the [`crate::app`] module; these are `impl App` blocks split out by concern.

use super::*;

impl App {
    /// Notify the LSP of changes to the active document (debounced by revision), and open
    /// documents the server hasn't seen. Inert unless a server is configured.
    pub(super) fn update_lsp(&mut self) {
        if !self.lsp.is_enabled() {
            return;
        }
        let Some(id) = self.editor.workspace.active_doc() else {
            return;
        };
        let Some(doc) = self.editor.workspace.documents.get(id) else {
            return;
        };
        let (Some(path), Some(lang)) = (doc.path.clone(), doc.language.clone()) else {
            return;
        };
        let rev = doc.revision;
        // Kick the connection into starting (non-blocking) and wait for the handshake before
        // sending anything: didOpen/didChange are illegal before `initialized`. Once the
        // connection is Running the first didOpen goes out with current text via the `None` arm.
        self.lsp.ensure_started(&lang);
        if !self.lsp.is_ready(&lang) {
            return;
        }
        // Serialize the rope only when we actually have something to send. This runs every
        // frame; materializing a multi-MB document to a String on each unchanged tick would be
        // needless allocation churn. Record the sent revision only on a real send.
        match self.lsp_sent_revision.get(&id).copied() {
            None => {
                let text = doc.to_string();
                if self.lsp.did_open(&path, &lang, &text) {
                    self.lsp_sent_revision.insert(id, rev);
                }
            }
            Some(sent) if sent != rev => {
                let text = doc.to_string();
                if self.lsp.did_change(&path, &lang, &text) {
                    self.lsp_sent_revision.insert(id, rev);
                }
            }
            _ => {}
        }
    }

    /// LSP position of the primary cursor: `(path, language, line, utf16_char)`.
    pub(super) fn lsp_position(&self) -> Option<(PathBuf, String, u32, u32)> {
        let doc = self.editor.active_document()?;
        let path = doc.path.clone()?;
        let lang = doc.language.clone()?;
        let head = doc.selections.primary().head;
        let (line, col) = doc.char_to_line_col(head);
        let text = doc.line_text(line);
        let text = text.trim_end_matches(['\n', '\r']);
        let char16 = editor_lsp::position::char_col_to_utf16(text, col);
        Some((path, lang, line as u32, char16))
    }

    /// Act on a high-level LSP event (response or notification).
    pub(super) fn handle_lsp_event(&mut self, event: crate::lsp::LspEvent) {
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
            LspEvent::CodeActions(actions) => {
                // Resolve each action's edit URIs to paths and hand the list to the code-action
                // plugin, which shows a picker and applies the chosen one.
                let actions = actions
                    .into_iter()
                    .map(|a| editor_plugin::LspCodeAction {
                        title: a.title,
                        edit: to_primitive_workspace_edit(a.edit),
                    })
                    .collect();
                self.editor
                    .pending_events
                    .push(editor_plugin::event::Event::LspCodeActions(actions));
            }
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
            LspEvent::Completion(items) => {
                // Broadcast to the `completion` plugin, which anchors + filters into a popup.
                let items = items.into_iter().map(to_primitive_completion).collect();
                self.editor
                    .pending_events
                    .push(editor_plugin::event::Event::LspCompletion(items));
            }
            LspEvent::Rename(edit) => {
                // Resolve each file's URI to a path and hand the edit to the `rename` plugin, which
                // forwards it back through `Host::apply_workspace_edit` (applied on drain).
                self.editor
                    .pending_events
                    .push(editor_plugin::event::Event::LspWorkspaceEdit(
                        to_primitive_workspace_edit(edit),
                    ));
            }
            LspEvent::Message(text) => {
                self.editor.status_message = Some(format!("LSP: {text}"));
            }
            LspEvent::ServerExited { lang } => {
                // The server (for this language) exited. Forget which docs we've synced so that,
                // once it restarts, `update_lsp` re-sends `didOpen` for each (resync, §3.9). The
                // per-doc version counter is not reset — versions stay monotonic.
                let ids: Vec<editor_core::DocId> = self
                    .editor
                    .workspace
                    .documents
                    .iter()
                    .filter(|(_, d)| d.language.as_deref() == Some(lang.as_str()))
                    .map(|(id, _)| id)
                    .collect();
                for id in ids {
                    self.lsp_sent_revision.remove(&id);
                }
            }
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
            LspEvent::Formatting(edits) => {
                // Apply whole-document formatting to the active doc through the same
                // Transaction pipeline as rename (one atomic group, invariant #1).
                let Some(path) = self.editor.active_document().and_then(|d| d.path.clone()) else {
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
    pub(super) fn apply_workspace_edit(&mut self, edit: editor_plugin::LspWorkspaceEdit) -> usize {
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

    /// Forward a plugin-queued LSP request to the manager, resolving the primary cursor position
    /// app-side (the `lsp` plugin only expresses intent via `Host::lsp_request`). Symbols need no
    /// cursor; the rest share the `lsp_position` lookup.
    pub(super) fn dispatch_lsp_request(&mut self, kind: editor_plugin::LspRequestKind) {
        use editor_plugin::LspRequestKind as K;
        // Whole-file / workspace requests need no cursor position. Match by ref so the
        // owned-String `WorkspaceSymbols` variant isn't partially moved out of `kind`.
        match &kind {
            K::DocumentSymbols => {
                self.request_document_symbols();
                return;
            }
            K::Formatting => {
                self.request_formatting();
                return;
            }
            K::WorkspaceSymbols(query) => {
                // Query the active file's language server (one server per language).
                if let Some(lang) = self
                    .editor
                    .active_document()
                    .and_then(|d| d.language.clone())
                {
                    self.lsp.request_workspace_symbols(&lang, query);
                }
                return;
            }
            _ => {}
        }
        let Some((p, l, line, ch)) = self.lsp_position() else {
            return;
        };
        match kind {
            K::Hover => self.lsp.request_hover(&p, &l, line, ch),
            K::Definition => self.lsp.request_definition(&p, &l, line, ch),
            K::Implementation => self.lsp.request_implementation(&p, &l, line, ch),
            K::TypeDefinition => self.lsp.request_type_definition(&p, &l, line, ch),
            K::Completion => self.lsp.request_completion(&p, &l, line, ch),
            K::SignatureHelp => self.lsp.request_signature_help(&p, &l, line, ch),
            K::DocumentHighlight => self.lsp.request_document_highlight(&p, &l, line, ch),
            K::CodeAction => self.lsp.request_code_action(&p, &l, line, ch),
            K::References => self.lsp.request_references(&p, &l, line, ch),
            K::Rename(name) => self.lsp.request_rename(&p, &l, line, ch, &name),
            K::DocumentSymbols | K::Formatting | K::WorkspaceSymbols(_) => false, // handled above
        };
    }

    /// Act on a server→client request that needs the editor (docs/UI) and answer it. Every arm
    /// MUST reply through `LspManager::respond` (§1.3): an unanswered request hangs the server.
    pub(super) fn handle_server_request(
        &mut self,
        lang: String,
        id: serde_json::Value,
        method: &str,
        params: serde_json::Value,
    ) {
        match method {
            "workspace/applyEdit" => {
                // A server-initiated edit (code actions / executeCommand results). Apply through
                // the same Transaction pipeline as rename (invariant #1) and report applied.
                let edit = params
                    .get("edit")
                    .map(editor_lsp::client::parse_workspace_edit)
                    .unwrap_or_default();
                let primitive = to_primitive_workspace_edit(edit);
                // Report what actually landed, not just what was requested (a stale/missing
                // target file changes nothing).
                let applied = self.apply_workspace_edit(primitive) > 0;
                self.lsp
                    .respond(&lang, &id, serde_json::json!({ "applied": applied }));
            }
            "window/showMessageRequest" => {
                // Surface the message; PR2 chooses no action (later: action buttons).
                if let Some(msg) = params.get("message").and_then(|m| m.as_str()) {
                    self.editor.status_message = Some(format!("LSP: {msg}"));
                }
                self.lsp.respond(&lang, &id, serde_json::Value::Null);
            }
            "window/showDocument" => {
                // Open a `file:` document in the editor unless the server asked for an external
                // (OS/browser) open, which we don't do here.
                let external = params
                    .get("external")
                    .and_then(|e| e.as_bool())
                    .unwrap_or(false);
                let path = params
                    .get("uri")
                    .and_then(|u| u.as_str())
                    .and_then(crate::lsp::path_from_uri);
                // Only claim success for a real openable file we actually show — not a directory
                // or a missing/external target.
                let opened = match (external, path) {
                    (false, Some(path)) if path.is_file() => {
                        self.open_path(&path);
                        true
                    }
                    _ => false,
                };
                self.lsp
                    .respond(&lang, &id, serde_json::json!({ "success": opened }));
            }
            // The manager only routes the three methods above; anything else still gets a reply
            // so a mis-route can never hang the server.
            _ => self.lsp.respond(&lang, &id, serde_json::Value::Null),
        }
    }

    /// Request the symbols in the active document (no cursor position needed).
    pub(super) fn request_document_symbols(&mut self) {
        let info = self.editor.active_document().and_then(|d| {
            let path = d.path.clone()?;
            let lang = d.language.clone()?;
            Some((path, lang))
        });
        if let Some((path, lang)) = info {
            self.lsp.request_document_symbols(&path, &lang);
        }
    }

    /// Request whole-document formatting for the active document, using the editor's indent
    /// settings as `FormattingOptions` (Lumina indents with spaces).
    pub(super) fn request_formatting(&mut self) {
        let info = self.editor.active_document().and_then(|d| {
            let path = d.path.clone()?;
            let lang = d.language.clone()?;
            Some((path, lang))
        });
        if let Some((path, lang)) = info {
            let tab_size = self.config.tab_width as u32;
            self.lsp.request_formatting(&path, &lang, tab_size, true);
        }
    }
}

/// Translate an `editor-lsp` diagnostic into the kernel's primitive `LspDiagnostic` (so the
/// `diagnostics` plugin owns the model without depending on `editor-lsp`).
fn to_primitive_diag(d: editor_lsp::Diagnostic) -> editor_plugin::LspDiagnostic {
    use editor_lsp::Severity as S;
    use editor_plugin::LspSeverity as P;
    editor_plugin::LspDiagnostic {
        line: d.line,
        start_char16: d.start_char16,
        end_line: d.end_line,
        end_char16: d.end_char16,
        severity: match d.severity {
            S::Error => P::Error,
            S::Warning => P::Warning,
            S::Info => P::Info,
            S::Hint => P::Hint,
        },
        message: d.message,
        source: d.source,
        code: d.code,
    }
}

/// Translate an `editor-lsp` completion item into the kernel's primitive `LspCompletionItem`.
fn to_primitive_completion(it: editor_lsp::CompletionItem) -> editor_plugin::LspCompletionItem {
    editor_plugin::LspCompletionItem {
        label: it.label,
        detail: it.detail,
        insert_text: it.insert_text,
        kind: it.kind,
    }
}

/// Translate an `editor-lsp` text edit into the kernel's primitive `LspTextEdit` (same coordinates).
fn to_primitive_text_edit(te: editor_lsp::TextEdit) -> editor_plugin::LspTextEdit {
    editor_plugin::LspTextEdit {
        start_line: te.start_line,
        start_char16: te.start_char16,
        end_line: te.end_line,
        end_char16: te.end_char16,
        new_text: te.new_text,
    }
}

/// Resolve an `editor-lsp` `WorkspaceEdit`'s URIs to filesystem paths and convert its edits to
/// the kernel primitive. Shared by rename responses and server-initiated `workspace/applyEdit`;
/// non-`file:` URIs are dropped.
fn to_primitive_workspace_edit(edit: editor_lsp::WorkspaceEdit) -> editor_plugin::LspWorkspaceEdit {
    let changes = edit
        .changes
        .into_iter()
        .filter_map(|(uri, edits)| {
            let path = crate::lsp::path_from_uri(&uri)?;
            let edits = edits.into_iter().map(to_primitive_text_edit).collect();
            Some((path.to_string_lossy().into_owned(), edits))
        })
        .collect();
    editor_plugin::LspWorkspaceEdit { changes }
}

/// Resolve an `editor-lsp` location's URI to a filesystem path and package it as the primitive
/// [`editor_plugin::LspLocation`] the `lsp-nav` plugin jumps to. `None` for a non-`file:` URI.
fn to_primitive_location(loc: &editor_lsp::Location) -> Option<editor_plugin::LspLocation> {
    let path = crate::lsp::path_from_uri(&loc.uri)?;
    Some(editor_plugin::LspLocation {
        path: path.to_string_lossy().into_owned(),
        line: loc.line,
        character: loc.character,
    })
}

/// A `file:line:col` label for a location picker row (plan §2.3).
fn location_label(loc: &editor_lsp::Location) -> String {
    let file = crate::lsp::path_from_uri(&loc.uri)
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| loc.uri.clone());
    format!("{file}:{}:{}", loc.line + 1, loc.character + 1)
}
