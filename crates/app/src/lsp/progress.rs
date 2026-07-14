//! Work-done progress (§1.5): the per-token model, folding `$/progress` notifications into the
//! active set, and rendering it as one statusline line.

use super::*;

/// One active work-done progress token (§1.5), keyed by `(lang, token)` and rendered as a
/// statusline segment. `title` is fixed at `begin`; `message`/`percentage` update on `report`.
pub(crate) struct ProgressItem {
    pub(crate) lang: String,
    pub(crate) token: String,
    pub(crate) title: String,
    pub(crate) message: Option<String>,
    pub(crate) percentage: Option<u32>,
}

impl ProgressItem {
    /// A one-line render: `lang: title — message 45%` (message/percentage omitted when absent).
    fn render(&self) -> String {
        let mut s = format!("{}: {}", self.lang, self.title);
        if let Some(m) = self.message.as_deref().filter(|m| !m.is_empty()) {
            s.push_str(" — ");
            s.push_str(m);
        }
        if let Some(p) = self.percentage {
            s.push_str(&format!(" {p}%"));
        }
        s
    }
}

impl LspManager {
    /// Fold a `$/progress` notification into the active work-done set and return the re-rendered
    /// statusline line (§1.5). Values without a `kind` are partial-result streams (we send no
    /// partial-result tokens yet) and are ignored → `None`. `begin` adds, `report` updates,
    /// `end` removes; the token is normalized to a string (it may arrive as a number).
    pub(crate) fn handle_progress(
        &mut self,
        lang: &str,
        params: &serde_json::Value,
    ) -> Option<LspEvent> {
        let token = progress_token(params.get("token")?);
        let value = params.get("value")?;
        let same = |p: &ProgressItem| p.lang == lang && p.token == token;
        match value.get("kind")?.as_str()? {
            "begin" => {
                self.progress.retain(|p| !same(p)); // a re-begun token replaces the old one
                self.progress.push(ProgressItem {
                    lang: lang.to_string(),
                    token,
                    title: value
                        .get("title")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string(),
                    message: value
                        .get("message")
                        .and_then(|v| v.as_str())
                        .map(String::from),
                    percentage: value
                        .get("percentage")
                        .and_then(|v| v.as_u64())
                        .map(|p| p as u32),
                });
            }
            "report" => {
                // A report for an unknown token has nothing to re-render → drop it (None).
                let item = self.progress.iter_mut().find(|p| same(p))?;
                // A report omitting a field leaves the prior value in place.
                if let Some(m) = value.get("message").and_then(|v| v.as_str()) {
                    item.message = Some(m.to_string());
                }
                if let Some(p) = value.get("percentage").and_then(|v| v.as_u64()) {
                    item.percentage = Some(p as u32);
                }
            }
            "end" => {
                let before = self.progress.len();
                self.progress.retain(|p| !same(p));
                if self.progress.len() == before {
                    return None; // end for an unknown token
                }
            }
            _ => return None,
        }
        Some(LspEvent::Progress(self.render_progress()))
    }

    /// The active progress tokens rendered as one statusline line (` · `-joined), or `None` when
    /// nothing is in flight (so the segment clears).
    pub(crate) fn render_progress(&self) -> Option<String> {
        if self.progress.is_empty() {
            return None;
        }
        let line = self
            .progress
            .iter()
            .map(ProgressItem::render)
            .collect::<Vec<_>>()
            .join(" · ");
        Some(line)
    }
}

/// Normalize a `$/progress` token to a string key — it may arrive as a JSON string or number.
fn progress_token(v: &serde_json::Value) -> String {
    v.as_str()
        .map(String::from)
        .unwrap_or_else(|| v.to_string())
}
