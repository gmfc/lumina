//! The read-only status view the LSP panel renders: one row per language the session has touched,
//! summarizing its server's state, resolved command, install hint (when missing), and last error.
//! Derived entirely from existing manager state (`state`/`failed`/`resolved`/`overrides`/
//! `last_error`) — a pure projection (invariant #8), no new bookkeeping.

use std::collections::BTreeSet;

use super::*;

/// Coarse per-language server state for the panel row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LangState {
    /// Handshake complete, serving requests.
    Running,
    /// Spawned, awaiting the initialize response.
    Starting,
    /// A server resolves on `$PATH` but no file has started it yet.
    Installed,
    /// Discovery probed the registry and found nothing installed.
    NotInstalled,
    /// The server crashed / the breaker tripped / the handshake failed.
    Crashed,
}

/// One language's line in the LSP panel.
pub(crate) struct LangStatus {
    pub(crate) lang: String,
    pub(crate) state: LangState,
    /// The resolved (or overridden) launch command, joined for display.
    pub(crate) command: Option<String>,
    /// A copy-paste install hint, present only when `NotInstalled`.
    pub(crate) install: Option<&'static str>,
    /// The most recent error, present only when `Crashed`.
    pub(crate) error: Option<String>,
}

impl LspManager {
    /// Cap on retained log lines per language.
    const LOG_CAP: usize = 500;

    /// One row per language the session has encountered (opened, configured, started, or failed),
    /// sorted by language id. Empty before any language-detected file is opened.
    pub(crate) fn status_rows(&self) -> Vec<LangStatus> {
        let mut langs: BTreeSet<&str> = BTreeSet::new();
        langs.extend(self.resolved.keys().map(String::as_str));
        langs.extend(self.state.keys().map(String::as_str));
        langs.extend(self.overrides.keys().map(String::as_str));
        langs.extend(self.failed.keys().map(String::as_str));

        langs
            .into_iter()
            .map(|lang| {
                // Resolved argv (or a configured override we haven't probed yet), for display.
                let command = match self.resolved.get(lang) {
                    Some(Some(argv)) => Some(argv.join(" ")),
                    _ => self.overrides.get(lang).map(|c| c.join(" ")),
                };
                let (state, install, error) = self.classify(lang);
                LangStatus {
                    lang: lang.to_string(),
                    state,
                    command,
                    install,
                    error,
                }
            })
            .collect()
    }

    /// Reduce a language's scattered state into `(state, install-hint, last-error)`.
    fn classify(&self, lang: &str) -> (LangState, Option<&'static str>, Option<String>) {
        match self.state.get(lang) {
            Some(ClientState::Running(_)) => (LangState::Running, None, None),
            Some(ClientState::Initializing { .. }) => (LangState::Starting, None, None),
            // No live connection: crashed/failed, not-installed, or resolved-but-idle.
            None if self.failed.contains_key(lang) => {
                (LangState::Crashed, None, self.last_error.get(lang).cloned())
            }
            None => match self.resolved.get(lang) {
                Some(None) => (LangState::NotInstalled, registry::install_hint(lang), None),
                _ => (LangState::Installed, None, None),
            },
        }
    }

    /// Whether `lang` has a *known* server (in the registry) that probed as not installed — the
    /// trigger for the one-time "install it" nudge. False for unknown languages (no server to
    /// install) and for anything installed/running.
    pub(crate) fn server_missing(&self, lang: &str) -> bool {
        matches!(self.resolved.get(lang), Some(None)) && registry::install_hint(lang).is_some()
    }

    /// Record the most recent error for `lang` (shown in the panel's status row).
    pub(super) fn set_last_error(&mut self, lang: &str, message: String) {
        self.last_error.insert(lang.to_string(), message);
    }

    /// Force a language's memoized resolution, so tests can exercise the not-installed path without
    /// depending on what happens to be on the real `$PATH`.
    #[cfg(test)]
    pub(crate) fn set_resolved_for_test(&mut self, lang: &str, argv: Option<Vec<String>>) {
        self.resolved.insert(lang.to_string(), argv);
    }

    /// Inject a log line, so a render test can populate the panel's log tail without a live server.
    #[cfg(test)]
    pub(crate) fn push_log_for_test(&mut self, lang: &str, line: &str) {
        self.push_log(lang, line.to_string());
    }

    /// Force a language into the `Running` state, so tests can exercise the server-ready paths
    /// (e.g. context-menu gating) without a live handshake.
    #[cfg(test)]
    pub(crate) fn set_ready_for_test(&mut self, lang: &str) {
        self.state.insert(
            lang.to_string(),
            ClientState::Running(ServerCaps::default()),
        );
    }

    /// Append a log line for `lang`, trimming the ring to the newest [`Self::LOG_CAP`] lines.
    pub(super) fn push_log(&mut self, lang: &str, line: String) {
        let ring = self.logs.entry(lang.to_string()).or_default();
        if ring.len() >= Self::LOG_CAP {
            ring.pop_front();
        }
        ring.push_back(line);
    }

    /// The most recent log lines for the LSP panel's log tail, each tagged with its language.
    /// Grouped by language (sorted) — cross-server order isn't meaningful without global timestamps,
    /// and a stable order keeps the panel from jumping around as lines arrive. Bounded by `limit`.
    pub(crate) fn recent_logs(&self, limit: usize) -> Vec<String> {
        let mut langs: Vec<&String> = self.logs.keys().collect();
        langs.sort();
        let mut lines: Vec<String> = langs
            .into_iter()
            .flat_map(|lang| self.logs[lang].iter().map(move |l| format!("[{lang}] {l}")))
            .collect();
        let start = lines.len().saturating_sub(limit);
        lines.drain(..start);
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_rows_summarize_each_touched_language() {
        let mut mgr = LspManager::new(std::path::Path::new("/tmp"), HashMap::new(), "test".into());
        mgr.enable_discovery();
        // A running server.
        mgr.state
            .insert("rust".into(), ClientState::Running(ServerCaps::default()));
        mgr.resolved
            .insert("rust".into(), Some(vec!["rust-analyzer".into()]));
        // A probed-but-not-installed language.
        mgr.resolved.insert("go".into(), None);
        // A crashed language with a recorded error.
        mgr.failed.insert("python".into(), ());
        mgr.set_last_error("python", "boom".into());

        let rows = mgr.status_rows();
        let by = |l: &str| rows.iter().find(|r| r.lang == l).unwrap();

        assert_eq!(by("rust").state, LangState::Running);
        assert_eq!(by("rust").command.as_deref(), Some("rust-analyzer"));

        assert_eq!(by("go").state, LangState::NotInstalled);
        assert!(by("go").install.unwrap().contains("gopls"));

        assert_eq!(by("python").state, LangState::Crashed);
        assert_eq!(by("python").error.as_deref(), Some("boom"));

        // Sorted by language id.
        let order: Vec<&str> = rows.iter().map(|r| r.lang.as_str()).collect();
        assert_eq!(order, ["go", "python", "rust"]);
    }

    #[test]
    fn status_rows_are_empty_before_any_language_is_touched() {
        let mgr = LspManager::new(std::path::Path::new("/tmp"), HashMap::new(), "test".into());
        assert!(mgr.status_rows().is_empty());
    }

    #[test]
    fn logs_are_bounded_and_tailed() {
        let mut mgr = LspManager::new(std::path::Path::new("/tmp"), HashMap::new(), "test".into());
        // Push more than the cap; only the newest LOG_CAP survive.
        for i in 0..(LspManager::LOG_CAP + 10) {
            mgr.push_log("rust", format!("line {i}"));
        }
        assert_eq!(mgr.logs["rust"].len(), LspManager::LOG_CAP);
        assert_eq!(mgr.logs["rust"].front().unwrap(), "line 10"); // oldest 10 dropped
                                                                  // recent_logs returns a bounded, language-tagged tail.
        let tail = mgr.recent_logs(3);
        assert_eq!(tail.len(), 3);
        assert!(tail[0].starts_with("[rust] line "));
    }

    #[test]
    fn recent_logs_group_by_language_in_sorted_order() {
        let mut mgr = LspManager::new(std::path::Path::new("/tmp"), HashMap::new(), "test".into());
        mgr.push_log("rust", "r1".into());
        mgr.push_log("go", "g1".into());
        // Deterministic: grouped by language, sorted (`go` before `rust`).
        assert_eq!(
            mgr.recent_logs(10),
            vec!["[go] g1".to_string(), "[rust] r1".to_string()]
        );
    }

    #[test]
    fn server_missing_only_for_known_uninstalled_languages() {
        let mut mgr = LspManager::new(std::path::Path::new("/tmp"), HashMap::new(), "test".into());
        // A known language probed as not installed → missing.
        mgr.set_resolved_for_test("go", None);
        assert!(mgr.server_missing("go"));
        // A known language that resolved → not missing.
        mgr.set_resolved_for_test("rust", Some(vec!["rust-analyzer".into()]));
        assert!(!mgr.server_missing("rust"));
        // An unknown language (no registry entry) → never "missing" (nothing to install).
        mgr.set_resolved_for_test("cobol", None);
        assert!(!mgr.server_missing("cobol"));
        // Never probed → not missing.
        assert!(!mgr.server_missing("python"));
    }
}
