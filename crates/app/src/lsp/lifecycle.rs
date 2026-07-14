//! Per-connection lifecycle: the handshake/serving state machine, spawn + restart policy
//! (crash breaker + exponential backoff), graceful shutdown, and the capability gates every
//! feature request is checked against.

use super::*;

/// Per-connection lifecycle gate. The Crashed terminal state is represented by removal from
/// `state` + an entry in `failed` (circuit breaker tripped); a live connection is either
/// Initializing or Running.
pub(crate) enum ClientState {
    /// `initialize` sent; awaiting the response (whose id is `init_id`) to store capabilities
    /// and send `initialized`.
    Initializing { init_id: i64 },
    /// Handshake complete; feature requests are gated on these capabilities.
    Running(ServerCaps),
}

/// A message from a connection's forwarding thread. `Exited` is synthesized when the server's
/// stdout closes (the process died), turning a silent crash into an observable event.
pub(crate) enum ClientMsg {
    Msg(Incoming),
    Exited,
}

/// Circuit breaker: if a server crashes this many times within [`CRASH_WINDOW`], stop
/// auto-restarting it *(≈ VS Code default)*.
pub(crate) const CRASH_LIMIT: usize = 5;
const CRASH_WINDOW: Duration = Duration::from_secs(180);

/// Whether the crash `times` (already pruned to the window) have hit the breaker limit.
pub(crate) fn breaker_tripped(times: &[Instant], now: Instant) -> bool {
    times
        .iter()
        .filter(|&&t| now.saturating_duration_since(t) <= CRASH_WINDOW)
        .count()
        >= CRASH_LIMIT
}

/// Exponential restart backoff for the Nth consecutive crash (1-based): 250 ms → 4 s.
pub(crate) fn restart_backoff(crash_count: usize) -> Duration {
    let shift = crash_count.saturating_sub(1).min(4);
    Duration::from_millis((250u64 << shift).min(4000))
}

impl LspManager {
    /// Finish (or abandon) the initialize handshake: on error drop the connection permanently; on
    /// success store capabilities, send `initialized`, and start serving.
    pub(crate) fn complete_handshake(
        &mut self,
        lang: &str,
        result: serde_json::Value,
        error: Option<ResponseError>,
        out: &mut Vec<LspEvent>,
    ) {
        match error {
            Some(err) => {
                self.clients.remove(lang);
                self.state.remove(lang);
                self.failed.insert(lang.to_string(), ());
                out.push(LspEvent::Error(format!(
                    "initialize failed: {}",
                    err.message
                )));
            }
            None => {
                let caps = parse_capabilities(&result);
                if let Some(handle) = self.clients.get(lang) {
                    let _ = handle.send_initialized();
                }
                self.state
                    .insert(lang.to_string(), ClientState::Running(caps));
            }
        }
    }

    /// Handle a connection's process exiting (§3.9). Fails its in-flight requests, clears its
    /// diagnostics, tells the app to re-sync, and applies the restart policy: auto-restart after
    /// exponential backoff unless the crash breaker has tripped ([`CRASH_LIMIT`] in
    /// [`CRASH_WINDOW`]).
    pub(crate) fn handle_exit(&mut self, lang: &str, out: &mut Vec<LspEvent>) {
        self.clients.remove(lang);
        self.state.remove(lang);

        // Fail in-flight requests locally — no response is coming.
        let dead: Vec<(String, i64)> = self
            .pending
            .keys()
            .filter(|(l, _)| l == lang)
            .cloned()
            .collect();
        let had_pending = !dead.is_empty();
        for key in dead {
            self.pending.remove(&key);
        }
        self.inflight.retain(|(l, _), _| l != lang);
        // The restarted server starts with an empty mirror — forget the old attach set (the app
        // replays didOpen for its docs on resync).
        self.open_docs.remove(lang);
        // Dynamic registrations are connection-scoped: the restarted server re-registers (§3.6).
        self.file_watchers.remove(lang);
        // Drop this server's in-flight progress and refresh the statusline segment (§1.5).
        let had_progress = self.progress.iter().any(|p| p.lang == lang);
        self.progress.retain(|p| p.lang != lang);
        if had_progress {
            out.push(LspEvent::Progress(self.render_progress()));
        }

        // Clear this server's stale diagnostics from the UI (and forget their raw + resultId
        // caches — the restarted server recomputes from scratch).
        if let Some(uris) = self.published.remove(lang) {
            for uri in uris {
                self.diag_raw.remove(&uri);
                self.diag_result_id.remove(&uri);
                self.code_lens.remove(&uri);
                out.push(LspEvent::Diagnostics(DiagnosticsUpdate {
                    uri,
                    diagnostics: Vec::new(),
                    raw: Vec::new(),
                }));
            }
        }

        // Let the app forget per-doc sync bookkeeping so docs re-open after a restart.
        out.push(LspEvent::ServerExited {
            lang: lang.to_string(),
        });
        if had_pending {
            out.push(LspEvent::Error(format!("{lang}: language server exited")));
        }

        // Restart policy: breaker + exponential backoff.
        let now = Instant::now();
        let times = self.crash_times.entry(lang.to_string()).or_default();
        times.retain(|&t| now.saturating_duration_since(t) <= CRASH_WINDOW);
        times.push(now);
        let count = times.len();
        if breaker_tripped(times, now) {
            self.failed.insert(lang.to_string(), ());
            self.restart_after.remove(lang);
            out.push(LspEvent::Error(format!(
                "{lang}: language server crashed {count} times; not restarting"
            )));
        } else {
            self.restart_after
                .insert(lang.to_string(), now + restart_backoff(count));
        }
    }

    /// Ensure a connection for `language` is at least started (spawned + `Initializing`).
    /// Returns whether a connection record now exists (initializing or running). Non-blocking:
    /// the handshake completes later in [`LspManager::poll`].
    pub(crate) fn ensure_started(&mut self, language: &str) -> bool {
        if self.clients.contains_key(language) {
            return true;
        }
        if self.failed.contains_key(language) {
            return false;
        }
        // Respect restart backoff after a crash: don't respawn until the cool-off passes.
        if let Some(after) = self.restart_after.get(language) {
            if Instant::now() < *after {
                return false;
            }
        }
        let Some(cmd) = self.resolve_server(language) else {
            return false;
        };
        let (program, args) = cmd
            .split_first()
            .map(|(p, a)| (p.clone(), a.to_vec()))
            .unzip();
        let Some(program) = program else {
            return false;
        };
        match LspClient::spawn(
            &program,
            &args.unwrap_or_default(),
            &self.root_uri,
            &self.client_version,
        ) {
            Ok((handle, rx, init_id)) => {
                // Forward this server's messages onto the shared channel, tagged with the
                // language so `poll` can route them (ids collide across connections). A synthetic
                // `Exited` is emitted when the stream closes so a crash becomes observable.
                let tx = self.tx.clone();
                let lang = language.to_string();
                std::thread::spawn(move || {
                    while let Ok(msg) = rx.recv() {
                        if tx.send((lang.clone(), ClientMsg::Msg(msg))).is_err() {
                            return;
                        }
                    }
                    let _ = tx.send((lang, ClientMsg::Exited));
                });
                self.restart_after.remove(language);
                self.clients.insert(language.to_string(), handle);
                self.state
                    .insert(language.to_string(), ClientState::Initializing { init_id });
                true
            }
            Err(_) => {
                self.failed.insert(language.to_string(), ());
                false
            }
        }
    }

    /// True once the handshake completed and the connection is serving requests.
    pub(crate) fn is_ready(&self, language: &str) -> bool {
        matches!(self.state.get(language), Some(ClientState::Running(_)))
    }

    /// Gate: the connection is `Running` and advertised support for `cap`.
    pub(crate) fn request_allowed(&self, language: &str, cap: Cap) -> bool {
        matches!(self.state.get(language), Some(ClientState::Running(caps)) if caps.allows(cap))
    }

    /// Whether the server declared `command` in `executeCommandProvider.commands` — only declared
    /// commands may be sent to `workspace/executeCommand` (§8.4).
    pub(crate) fn can_execute(&self, language: &str, command: &str) -> bool {
        matches!(self.state.get(language), Some(ClientState::Running(caps))
            if caps.execute_commands.iter().any(|c| c == command))
    }

    /// Gracefully stop all connections on quit, running the per-server teardowns **concurrently**
    /// under one global deadline (§3.8): every server gets the full `deadline` in parallel, so a
    /// hung server can't make quit wait `deadline × N` — total quit time stays ~`deadline`.
    pub fn stop_all(&mut self, deadline: Duration) {
        let threads: Vec<_> = self
            .clients
            .drain()
            .map(|(_lang, mut client)| std::thread::spawn(move || client.stop(deadline)))
            .collect();
        for t in threads {
            let _ = t.join();
        }
        self.state.clear();
    }

    /// Whether the app should engage the LSP layer at all. True when discovery is on (the built-in
    /// registry can serve many languages) or any explicit override is configured. Whether a *given*
    /// file gets a server is decided lazily per language in [`Self::resolve_server`].
    pub fn is_enabled(&self) -> bool {
        self.discover || !self.overrides.is_empty()
    }

    /// A coarse health tag for `lang`'s connection (the active file's language), for the footer
    /// status indicator. `""` = no server applies; `"starting"` during the handshake; `"ready"`
    /// when serving; `"error"` when the connection failed / the crash breaker tripped. Stringly
    /// typed on purpose: it is mirrored into `editor.status_items` for the renderer, which cannot
    /// read the private manager field.
    pub(crate) fn health_tag_for(&self, lang: Option<&str>) -> &'static str {
        let Some(lang) = lang else {
            return "";
        };
        match self.state.get(lang) {
            Some(ClientState::Initializing { .. }) => "starting",
            Some(ClientState::Running(_)) => "ready",
            None if self.failed.contains_key(lang) => "error",
            None => "",
        }
    }

    /// Turn on zero-config discovery (production). Off by default so tests never auto-spawn a real
    /// language server that happens to be on `$PATH`.
    pub(crate) fn enable_discovery(&mut self) {
        self.discover = true;
    }

    /// Turn discovery off (used by the test harness to keep the App hermetic).
    #[cfg(test)]
    pub(crate) fn disable_discovery(&mut self) {
        self.discover = false;
        self.resolved.clear();
    }

    /// Resolve `language` to an installed server argv, memoizing the probe. An explicit `[lsp]`
    /// override is honored verbatim and always wins; otherwise, when discovery is on, the built-in
    /// [`registry`] candidates are probed against `$PATH` and the first installed one wins.
    /// `None` = no server for this language (unknown, not installed, or discovery off).
    pub(crate) fn resolve_server(&mut self, language: &str) -> Option<Vec<String>> {
        if let Some(cached) = self.resolved.get(language) {
            return cached.clone();
        }
        let resolution = self.probe_server(language);
        self.resolved
            .insert(language.to_string(), resolution.clone());
        resolution
    }

    fn probe_server(&self, language: &str) -> Option<Vec<String>> {
        if let Some(cmd) = self.overrides.get(language) {
            return Some(cmd.clone());
        }
        if !self.discover {
            return None;
        }
        let def = registry::registry().get(language)?;
        registry::first_installed(def.candidates, registry::program_on_path)
    }

    /// Whether `language`'s server is `Running` and advertised pull diagnostics (§5.1) — the gate
    /// for the app's debounced `textDocument/diagnostic` polling.
    pub fn supports_pull(&self, language: &str) -> bool {
        self.request_allowed(language, Cap::PullDiagnostics)
    }

    /// Whether `language`'s server is `Running` and advertised full-document semantic tokens
    /// (§7.1) — the gate for the app requesting them alongside each doc sync.
    pub fn supports_semantic_tokens(&self, language: &str) -> bool {
        self.request_allowed(language, Cap::SemanticTokens)
    }

    /// Whether `language`'s server is `Running` and advertised inlay hints (§7.2).
    pub fn supports_inlay_hints(&self, language: &str) -> bool {
        self.request_allowed(language, Cap::InlayHint)
    }

    /// Whether `language`'s server is `Running` and advertised code lens (§6.4).
    pub fn supports_code_lens(&self, language: &str) -> bool {
        self.request_allowed(language, Cap::CodeLens)
    }

    /// Whether `language`'s server is `Running` and advertised folding ranges (§7.3).
    pub fn supports_folding(&self, language: &str) -> bool {
        self.request_allowed(language, Cap::FoldingRange)
    }
}
