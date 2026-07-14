//! Diagnostics (push + pull) and code lenses: tracking published diagnostics for crash-clearing,
//! snapshotting their raw JSON for codeAction context, resolving pull reports, and accumulating
//! resolved code lenses per document.

use super::*;

impl LspManager {
    /// A `publishDiagnostics` push: track the URI for crash-clearing, snapshot its raw diagnostics
    /// (for codeAction context), and surface the update.
    pub(crate) fn on_diagnostics(
        &mut self,
        lang: &str,
        u: DiagnosticsUpdate,
        out: &mut Vec<LspEvent>,
    ) {
        self.published
            .entry(lang.to_string())
            .or_default()
            .insert(u.uri.clone());
        if u.raw.is_empty() {
            self.diag_raw.remove(&u.uri);
        } else {
            self.diag_raw.insert(u.uri.clone(), u.raw.clone());
        }
        out.push(LspEvent::Diagnostics(u));
    }

    /// Turn a pull-diagnostics report into the same bookkeeping + event a push would produce
    /// (§5.1). A `full` report replaces the URI's diagnostics (and raw snapshot) and caches its
    /// resultId; an `unchanged` report only refreshes the cached resultId (the UI keeps what it
    /// has). Reusing the push path means pulled diagnostics render + feed codeAction context
    /// identically.
    pub(crate) fn handle_pull_report(
        &mut self,
        uri: &str,
        lang: &str,
        result: &serde_json::Value,
        out: &mut Vec<LspEvent>,
    ) {
        match parse_diagnostic_report(result) {
            PullReport::Full {
                result_id,
                diagnostics,
                raw,
            } => {
                match result_id {
                    Some(rid) => {
                        self.diag_result_id.insert(uri.to_string(), rid);
                    }
                    None => {
                        self.diag_result_id.remove(uri);
                    }
                }
                self.published
                    .entry(lang.to_string())
                    .or_default()
                    .insert(uri.to_string());
                if raw.is_empty() {
                    self.diag_raw.remove(uri);
                } else {
                    self.diag_raw.insert(uri.to_string(), raw.clone());
                }
                out.push(LspEvent::Diagnostics(DiagnosticsUpdate {
                    uri: uri.to_string(),
                    diagnostics,
                    raw,
                }));
            }
            PullReport::Unchanged { result_id } => {
                if let Some(rid) = result_id {
                    self.diag_result_id.insert(uri.to_string(), rid);
                }
            }
        }
    }

    /// Handle a `textDocument/codeLens` response (§6.4): reset the per-uri lens set to the lenses
    /// that arrived already resolved (with a `title`), fire `codeLens/resolve` for the rest (when
    /// the server resolves lazily), and emit the resolved-so-far set. Later resolves append + re-emit.
    pub(crate) fn handle_code_lens(
        &mut self,
        uri: &str,
        lang: &str,
        result: &serde_json::Value,
        out: &mut Vec<LspEvent>,
    ) {
        let mut resolved = Vec::new();
        let mut to_resolve = Vec::new();
        for lens in parse_code_lenses(result) {
            if lens.title.is_some() {
                resolved.push(lens);
            } else {
                to_resolve.push(lens.raw);
            }
        }
        self.code_lens.insert(uri.to_string(), resolved.clone());
        let can_resolve = matches!(self.state.get(lang),
            Some(ClientState::Running(caps)) if caps.code_lens_resolve);
        if can_resolve {
            for raw in to_resolve {
                self.request_code_lens_resolve(lang, uri, &raw);
            }
        }
        out.push(LspEvent::CodeLenses {
            uri: uri.to_string(),
            lenses: resolved,
        });
    }

    /// Handle a `codeLens/resolve` response: append the now-titled lens to the uri's set and re-emit.
    pub(crate) fn handle_code_lens_resolve(
        &mut self,
        uri: &str,
        result: &serde_json::Value,
        out: &mut Vec<LspEvent>,
    ) {
        let Some(lens) = parse_code_lens_resolve(result).filter(|l| l.title.is_some()) else {
            return;
        };
        let set = self.code_lens.entry(uri.to_string()).or_default();
        set.push(lens);
        out.push(LspEvent::CodeLenses {
            uri: uri.to_string(),
            lenses: set.clone(),
        });
    }

    /// The raw diagnostics for `uri` whose range overlaps the (LSP, UTF-16) range `[start, end]`,
    /// to echo into a `codeAction` request's `context.diagnostics` (§6.1) so the server can offer
    /// quickfixes bound to them.
    pub(crate) fn context_diagnostics(
        &self,
        uri: &str,
        start: (u32, u32),
        end: (u32, u32),
    ) -> Vec<serde_json::Value> {
        self.diag_raw
            .get(uri)
            .map(|ds| {
                ds.iter()
                    .filter(|d| diag_overlaps(d, start, end))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// Whether a raw diagnostic's `range` intersects the (LSP, UTF-16) range `[start, end]` — the
/// test for including it in a `codeAction` request's `context.diagnostics` (§6.1). Positions are
/// compared as lexicographic `(line, character)` tuples; a missing/malformed range means "don't
/// include" (it can't be positioned against the request).
pub(crate) fn diag_overlaps(raw: &serde_json::Value, start: (u32, u32), end: (u32, u32)) -> bool {
    let pos = |obj: &serde_json::Value, key: &str| -> Option<(u32, u32)> {
        let p = obj.get(key)?;
        Some((
            p.get("line")?.as_u64()? as u32,
            p.get("character")?.as_u64()? as u32,
        ))
    };
    let Some(range) = raw.get("range") else {
        return false;
    };
    let (Some(ds), Some(de)) = (pos(range, "start"), pos(range, "end")) else {
        return false;
    };
    // Intersection of [ds, de] with [start, end] in lexicographic (line, char) order. Touching
    // endpoints count (a point request on a diagnostic boundary still surfaces its fix).
    ds <= end && start <= de
}
