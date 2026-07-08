//! Context construction and capability-gated action dispatch for [`ScriptPlugin`].

use std::path::Path;

use rhai::{Array, Map};

use super::actions::{insert_at_cursor, replace_line, replace_selection};
use super::manifest::{panel_from_map, str_field};
use super::ScriptPlugin;
use crate::Host;

impl ScriptPlugin {
    fn has_cap(&self, cap: &str) -> bool {
        self.capabilities.iter().any(|c| c == cap)
    }

    /// Read-only context handed to the script: cursor position + relevant text.
    pub(super) fn build_ctx(host: &dyn Host) -> Map {
        let mut ctx = Map::new();
        if let Some(doc) = host.workspace().active_document() {
            let head = doc.selections.primary().head;
            let (line, col) = doc.char_to_line_col(head);
            let line_text = doc.line_text(line);
            let line_text = line_text.trim_end_matches(['\n', '\r']).to_string();
            let sel = doc.selections.primary();
            let sel_text = if sel.is_empty() {
                String::new()
            } else {
                doc.text.slice(sel.from()..sel.to()).to_string()
            };
            ctx.insert("cursor_line".into(), (line as i64).into());
            ctx.insert("cursor_col".into(), (col as i64).into());
            ctx.insert("line_text".into(), line_text.into());
            ctx.insert("selection_text".into(), sel_text.into());
            ctx.insert("doc_text".into(), doc.to_string().into());
        }
        ctx
    }

    /// Apply the actions a script returned, gated by granted capabilities.
    pub(super) fn apply_actions(
        &self,
        panel_ctx: Option<&str>,
        actions: Array,
        host: &mut dyn Host,
    ) {
        for action in actions {
            if let Some(map) = action.try_cast::<Map>() {
                self.apply_action(panel_ctx, &map, host);
            }
        }
    }

    /// Dispatch a single action map, gated by granted capabilities.
    fn apply_action(&self, panel_ctx: Option<&str>, map: &Map, host: &mut dyn Host) {
        let kind = map
            .get("action")
            .and_then(|d| d.clone().into_string().ok())
            .unwrap_or_default();
        match kind.as_str() {
            "insert" | "replace_selection" | "replace_line" if self.has_cap("edit") => {
                self.apply_edit(&kind, map, host);
            }
            "notify" if self.has_cap("ui") => {
                if let Some(msg) = str_field(map, "message") {
                    host.notify(msg);
                }
            }
            "open" if self.has_cap("fs:read") => {
                if let Some(path) = str_field(map, "path") {
                    host.open_path(Path::new(&path));
                }
            }
            "run" => {
                if let Some(cmd) = str_field(map, "command") {
                    host.execute(&cmd);
                }
            }
            "set_panel" if self.has_cap("ui") => self.apply_set_panel(panel_ctx, map, host),
            _ => {}
        }
    }

    /// The three `edit`-gated text mutations, which all read a `text` field.
    fn apply_edit(&self, kind: &str, map: &Map, host: &mut dyn Host) {
        let Some(text) = str_field(map, "text") else {
            return;
        };
        match kind {
            "insert" => insert_at_cursor(host, &text),
            "replace_selection" => replace_selection(host, &text),
            "replace_line" => replace_line(host, &text),
            _ => {}
        }
    }

    fn apply_set_panel(&self, panel_ctx: Option<&str>, map: &Map, host: &mut dyn Host) {
        let panel_id = str_field(map, "panel").or_else(|| panel_ctx.map(str::to_string));
        if let Some(panel_id) = panel_id {
            let content = panel_from_map(map);
            host.set_panel(&panel_id, content);
        }
    }
}
