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

    /// Record the most recent error for `lang` (shown in the panel's status row).
    pub(super) fn set_last_error(&mut self, lang: &str, message: String) {
        self.last_error.insert(lang.to_string(), message);
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
}
