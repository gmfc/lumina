//! Response correlation: classifying a pending request, matching an incoming response back to it
//! (dropping superseded/stale ones), and turning a successful result into a high-level
//! [`LspEvent`].

use super::*;

/// What a pending request was, so its response can be interpreted when it arrives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Pending {
    Hover,
    Definition,
    Completion,
    Rename,
    References,
    DocumentSymbols,
    Formatting,
    SignatureHelp,
    DocumentHighlight,
    WorkspaceSymbols,
    CodeAction,
    ResolveCompletion,
    Diagnostic,
    SemanticTokens,
    InlayHint,
    CodeLens,
    CodeLensResolve,
    FoldingRange,
}

/// Whether a request kind is auto-cancelled when a newer one of the same kind supersedes it
/// (§9.3): passive, cursor-driven lookups are; explicit user actions (rename, references,
/// symbols) run to completion.
pub(crate) fn is_cancelable(kind: Pending) -> bool {
    matches!(
        kind,
        Pending::Hover
            | Pending::Definition
            | Pending::Completion
            | Pending::SignatureHelp
            | Pending::DocumentHighlight
            // A newer full-doc token / hint / lens / fold request supersedes the older one.
            | Pending::SemanticTokens
            | Pending::InlayHint
            | Pending::CodeLens
            | Pending::FoldingRange
    )
}

/// A pending request tagged for staleness detection (§9.4): the document + the version it was
/// asked against, so a response that arrives after the buffer moved can be dropped.
pub(crate) struct PendingEntry {
    pub(crate) kind: Pending,
    pub(crate) uri: String,
    pub(crate) version: i64,
}

impl LspManager {
    /// A response to one of our requests: complete the handshake if it is the awaited
    /// `InitializeResult`, otherwise correlate it to its pending request and dispatch the result.
    pub(crate) fn on_response(
        &mut self,
        lang: &str,
        id: i64,
        result: serde_json::Value,
        error: Option<ResponseError>,
        out: &mut Vec<LspEvent>,
    ) {
        let is_init = matches!(
            self.state.get(lang),
            Some(ClientState::Initializing { init_id }) if *init_id == id
        );
        if is_init {
            self.complete_handshake(lang, result, error, out);
            return;
        }
        if let Some(entry) = self.correlate_response(lang, id, error, out) {
            self.dispatch_response(lang, &entry, &result, out);
        }
    }

    /// Correlate a non-init response to its pending request, returning the entry only when the
    /// result is worth dispatching. Drops superseded (§9.4) and version-stale answers silently,
    /// and surfaces a real (non-droppable) error instead of a result.
    fn correlate_response(
        &mut self,
        lang: &str,
        id: i64,
        error: Option<ResponseError>,
        out: &mut Vec<LspEvent>,
    ) -> Option<PendingEntry> {
        let entry = self.pending.remove(&(lang.to_string(), id))?;
        let key = (lang.to_string(), entry.kind);
        let superseded = is_cancelable(entry.kind)
            && self.inflight.get(&key).is_some_and(|&latest| latest != id);
        if !superseded {
            self.inflight.remove(&key); // this request resolved its kind's slot
        }
        if let Some(err) = error {
            if !superseded && !err.is_droppable() {
                out.push(LspEvent::Error(err.message));
            }
            return None;
        }
        // Drop a result computed against a buffer that has since moved (§9.4).
        let current = self
            .versions
            .get(&entry.uri)
            .copied()
            .unwrap_or(entry.version);
        (!superseded && current == entry.version).then_some(entry)
    }

    /// Turn a correlated response into the right event: pull diagnostics, code lens, and semantic
    /// tokens need extra manager state; everything else maps straight through [`response_event`].
    fn dispatch_response(
        &mut self,
        lang: &str,
        entry: &PendingEntry,
        result: &serde_json::Value,
        out: &mut Vec<LspEvent>,
    ) {
        match entry.kind {
            Pending::Diagnostic => self.handle_pull_report(&entry.uri, lang, result, out),
            Pending::CodeLens => self.handle_code_lens(&entry.uri, lang, result, out),
            Pending::CodeLensResolve => self.handle_code_lens_resolve(&entry.uri, result, out),
            Pending::SemanticTokens => {
                // Decode against this connection's legend (fixed at capability time).
                let tokens = match self.state.get(lang) {
                    Some(ClientState::Running(caps)) => {
                        parse_semantic_tokens(result, &caps.semantic_legend)
                    }
                    _ => Vec::new(),
                };
                out.push(LspEvent::SemanticTokens {
                    uri: entry.uri.clone(),
                    tokens,
                });
            }
            _ => out.extend(response_event(entry.kind, &entry.uri, result)),
        }
    }
}

/// Interpret a successful response `result` against the request `kind` that produced it. Returns
/// `None` when the payload carries nothing to act on (an empty hover / no definition), so the
/// caller simply drops it.
fn response_event(kind: Pending, uri: &str, result: &serde_json::Value) -> Option<LspEvent> {
    Some(match kind {
        Pending::Hover => LspEvent::Hover(parse_hover(result)?),
        Pending::Definition => LspEvent::Goto(parse_locations(result).into_iter().next()?),
        Pending::Completion => LspEvent::Completion(parse_completion(result)),
        Pending::ResolveCompletion => LspEvent::CompletionResolvedEdits {
            uri: uri.to_string(),
            edits: parse_completion_item_additional_edits(result),
        },
        Pending::Rename => LspEvent::Rename(parse_workspace_edit(result)),
        Pending::References => LspEvent::References(parse_locations(result)),
        Pending::DocumentSymbols => LspEvent::DocumentSymbols(parse_document_symbols(result)),
        Pending::WorkspaceSymbols => LspEvent::WorkspaceSymbols(parse_workspace_symbols(result)),
        Pending::Formatting => LspEvent::Formatting {
            uri: uri.to_string(),
            edits: parse_text_edits(result),
        },
        // Always emit (even `None`) so the statusline hint clears when the cursor leaves a call.
        Pending::SignatureHelp => {
            LspEvent::SignatureHelp(parse_signature_help(result).map(|s| format_signature(&s)))
        }
        // Always emit (even empty) so highlights clear when the cursor leaves a symbol.
        Pending::DocumentHighlight => LspEvent::Highlights(parse_document_highlights(result)),
        Pending::CodeAction => LspEvent::CodeActions(parse_code_actions(result)),
        Pending::InlayHint => LspEvent::InlayHints {
            uri: uri.to_string(),
            hints: parse_inlay_hints(result),
        },
        Pending::FoldingRange => LspEvent::FoldingRanges {
            uri: uri.to_string(),
            ranges: parse_folding_ranges(result),
        },
        // Pull-diagnostics reports are handled in `poll` (they need the resultId cache), never here.
        Pending::Diagnostic => return None,
        // Semantic tokens are decoded in `poll` (they need the connection legend), never here.
        Pending::SemanticTokens => return None,
        // Code lenses accumulate resolve responses in `poll` (they need the per-uri set), not here.
        Pending::CodeLens | Pending::CodeLensResolve => return None,
    })
}

/// Render a signature line for the statusline, marking the active parameter with brackets.
pub(crate) fn format_signature(sig: &SignatureHelp) -> String {
    match sig.active_param {
        Some((s, e)) => {
            let chars: Vec<char> = sig.label.chars().collect();
            if s <= e && e <= chars.len() {
                let pre: String = chars[..s].iter().collect();
                let mid: String = chars[s..e].iter().collect();
                let post: String = chars[e..].iter().collect();
                format!("{pre}[{mid}]{post}")
            } else {
                sig.label.clone()
            }
        }
        None => sig.label.clone(),
    }
}
