//! Inbound server→client traffic: requests we must answer (configuration, workspace folders,
//! dynamic (un)registration, refresh acks) and notifications, plus forwarding project-tree
//! changes to registered file watchers and the raw reply helper.

use super::*;

impl LspManager {
    /// Answer a server→client request (§1.3): manager-local ones (configuration, folders, dynamic
    /// registration, refresh acks) here; applyEdit/showMessage(Request)/showDocument route to the
    /// app; a refresh also emits an event so the app re-requests. Unknown methods get `-32601`.
    pub(crate) fn on_server_request(
        &mut self,
        lang: &str,
        id: serde_json::Value,
        method: String,
        params: serde_json::Value,
        out: &mut Vec<LspEvent>,
    ) {
        // The refresh methods all ack + emit an app-facing event; table-drive them.
        let refresh = |lang: &str| -> Option<LspEvent> {
            match method.as_str() {
                "workspace/codeLens/refresh" => Some(LspEvent::CodeLensRefresh {
                    lang: lang.to_string(),
                }),
                "workspace/semanticTokens/refresh" => Some(LspEvent::SemanticTokensRefresh {
                    lang: lang.to_string(),
                }),
                "workspace/inlayHint/refresh" => Some(LspEvent::InlayHintRefresh {
                    lang: lang.to_string(),
                }),
                _ => None,
            }
        };
        match method.as_str() {
            "workspace/configuration" => self.respond(lang, &id, configuration_response(&params)),
            "workspace/workspaceFolders" => {
                self.respond(lang, &id, workspace_folders_response(&self.root_uri))
            }
            "client/registerCapability" => {
                self.register_capability(lang, &params);
                self.respond(lang, &id, serde_json::Value::Null);
            }
            "client/unregisterCapability" => {
                self.unregister_capability(lang, &params);
                self.respond(lang, &id, serde_json::Value::Null);
            }
            "window/workDoneProgress/create" => self.respond(lang, &id, serde_json::Value::Null),
            "workspace/diagnostic/refresh" => {
                // Also drop cached resultIds so the re-pull is a full one (§5.1).
                self.respond(lang, &id, serde_json::Value::Null);
                self.diag_result_id.clear();
                out.push(LspEvent::DiagnosticsRefresh {
                    lang: lang.to_string(),
                });
            }
            "workspace/applyEdit" | "window/showMessageRequest" | "window/showDocument" => out
                .push(LspEvent::ServerRequest {
                    lang: lang.to_string(),
                    id,
                    method,
                    params,
                }),
            _ => match refresh(lang) {
                Some(ev) => {
                    self.respond(lang, &id, serde_json::Value::Null);
                    out.push(ev);
                }
                None => {
                    if let Some(h) = self.clients.get(lang) {
                        let _ = h.respond_err(&id, -32601, "method not found");
                    }
                }
            },
        }
    }

    /// A server→client notification: `window/showMessage` → statusline, `$/progress` → the
    /// work-done spinner (§1.5). Everything else (logMessage / telemetry / unknown) is dropped.
    pub(crate) fn on_notification(
        &mut self,
        lang: &str,
        method: &str,
        params: &serde_json::Value,
        out: &mut Vec<LspEvent>,
    ) {
        if method == "window/showMessage" {
            if let Some(msg) = params.get("message").and_then(|m| m.as_str()) {
                out.push(LspEvent::Message(msg.to_string()));
            }
        } else if method == "$/progress" {
            if let Some(ev) = self.handle_progress(lang, params) {
                out.push(ev);
            }
        }
    }

    /// Store dynamic capability registrations for `language` (§3.6). Today only
    /// `workspace/didChangeWatchedFiles` is acted on — its watchers are compiled and kept by
    /// registration id; other methods are accepted (and answered `null` by the caller) but not yet
    /// routed. Registrations are connection-scoped (cleared on exit).
    pub(crate) fn register_capability(&mut self, language: &str, params: &serde_json::Value) {
        let Some(regs) = params.get("registrations").and_then(|r| r.as_array()) else {
            return;
        };
        for reg in regs {
            let (Some(id), Some(method)) = (
                reg.get("id").and_then(|v| v.as_str()),
                reg.get("method").and_then(|v| v.as_str()),
            ) else {
                continue;
            };
            if method == "workspace/didChangeWatchedFiles" {
                let opts = reg
                    .get("registerOptions")
                    .unwrap_or(&serde_json::Value::Null);
                let ws = watchers::parse_watchers(opts);
                if !ws.is_empty() {
                    self.file_watchers
                        .entry(language.to_string())
                        .or_default()
                        .insert(id.to_string(), ws);
                }
            }
        }
    }

    /// Remove dynamic registrations by id (§3.6). The spec misspells the key as `unregisterations`
    /// — deserialize that exact key.
    pub(crate) fn unregister_capability(&mut self, language: &str, params: &serde_json::Value) {
        let Some(unregs) = params.get("unregisterations").and_then(|r| r.as_array()) else {
            return;
        };
        if let Some(by_id) = self.file_watchers.get_mut(language) {
            for u in unregs {
                if let Some(id) = u.get("id").and_then(|v| v.as_str()) {
                    by_id.remove(id);
                }
            }
        }
    }

    /// Forward a project-tree change to every server that dynamically registered a matching
    /// watcher (§8.1). The change type is inferred from the path's current existence — the
    /// editor's watcher doesn't distinguish create from modify, so a freshly created file is
    /// reported as `Changed`, which servers treat the same (they re-read the file). Sent only to
    /// `Running` connections; no-op when no watchers are registered.
    pub fn notify_watched_file_change(&self, path: &Path) {
        if self.file_watchers.is_empty() {
            return;
        }
        let change_type = if path.exists() {
            watchers::CHANGED
        } else {
            watchers::DELETED
        };
        for (lang, by_id) in &self.file_watchers {
            if !self.is_ready(lang) {
                continue;
            }
            if watchers::any_match(by_id.values().flatten(), path, change_type) {
                if let Some(client) = self.clients.get(lang) {
                    let change = serde_json::json!({
                        "uri": uri_for(path),
                        "type": change_type,
                    });
                    let _ = client.did_change_watched_files(&[change]);
                }
            }
        }
    }

    /// Answer a server→client request for `language`, echoing its raw `id`. Public so the app
    /// can reply after acting on a routed [`LspEvent::ServerRequest`].
    pub fn respond(&self, language: &str, id: &serde_json::Value, result: serde_json::Value) {
        if let Some(client) = self.clients.get(language) {
            let _ = client.respond(id, result);
        }
    }
}

/// Build the response to `workspace/configuration`: one entry per requested item. We hold no
/// per-server settings yet, so every entry is `null` — but the arity **must** match the request
/// (a wrong-arity response wedges servers like pyright, §3.7).
pub(crate) fn configuration_response(params: &serde_json::Value) -> serde_json::Value {
    let n = params
        .get("items")
        .and_then(|i| i.as_array())
        .map(|a| a.len())
        .unwrap_or(0);
    serde_json::Value::Array(vec![serde_json::Value::Null; n])
}

/// Build the response to `workspace/workspaceFolders`: the single project root (§8.2).
pub(crate) fn workspace_folders_response(root_uri: &str) -> serde_json::Value {
    serde_json::json!([{ "uri": root_uri, "name": folder_name(root_uri) }])
}

/// The last path segment of a `file://` root URI, used as the workspace-folder name.
fn folder_name(root_uri: &str) -> String {
    root_uri
        .trim_end_matches('/')
        .rsplit('/')
        .find(|s| !s.is_empty())
        .unwrap_or("root")
        .to_string()
}
