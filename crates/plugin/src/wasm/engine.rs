//! WebAssembly execution internals for [`WasmPlugin`]: context construction, guest calls, and
//! capability-gated action dispatch.

use std::path::Path;

use serde_json::{json, Value};
use wasmi::{Linker, Store};

use super::WasmPlugin;
use crate::runtime;
use crate::Host;

/// Per-call fuel budget — generous for real work, finite so an infinite loop can't hang the UI.
const FUEL: u64 = 50_000_000;

impl WasmPlugin {
    pub(super) fn has_cap(&self, cap: &str) -> bool {
        self.capabilities.iter().any(|c| c == cap)
    }

    /// Read-only context handed to the guest (same shape as the Rhai tier's).
    pub(super) fn build_ctx(host: &dyn Host) -> Value {
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
            json!({
                "cursor_line": line,
                "cursor_col": col,
                "line_text": line_text,
                "selection_text": sel_text,
                "doc_text": doc.to_string(),
            })
        } else {
            json!({})
        }
    }

    /// Instantiate fresh (isolated state + fuel), call `func`, and return the parsed JSON it
    /// wrote to memory. Returns `None` on trap, out-of-fuel, or malformed output.
    pub(super) fn call(&self, func: &str, ctx: &Value) -> Option<Value> {
        let mut store = Store::new(&self.engine, ());
        store.set_fuel(FUEL).ok()?;
        let instance = Linker::new(&self.engine)
            .instantiate_and_start(&mut store, &self.module)
            .ok()?;
        let memory = instance.get_memory(&store, "memory")?;
        let entry = instance
            .get_typed_func::<(i32, i32), i64>(&store, func)
            .ok()?;

        // Hand the context to the guest only if it provides an allocator; else pass (0, 0).
        let (ptr, len) = match instance.get_typed_func::<i32, i32>(&store, "alloc") {
            Ok(alloc) => {
                let bytes = serde_json::to_vec(ctx).ok()?;
                let p = alloc.call(&mut store, bytes.len() as i32).ok()?;
                memory.write(&mut store, p as usize, &bytes).ok()?;
                (p, bytes.len() as i32)
            }
            Err(_) => (0, 0),
        };

        let packed = entry.call(&mut store, (ptr, len)).ok()?;
        let out_ptr = (packed >> 32) as usize;
        let out_len = (packed & 0xffff_ffff) as usize;
        if out_len == 0 {
            return None;
        }
        let data = memory.data(&store).get(out_ptr..out_ptr + out_len)?;
        serde_json::from_slice(data).ok()
    }

    /// Apply the actions a guest returned, gated by granted capabilities (mirrors the Rhai
    /// tier so both substrates enforce the same policy).
    pub(super) fn apply_actions(&self, actions: &Value, host: &mut dyn Host) {
        let Some(arr) = actions.as_array() else {
            return;
        };
        for action in arr {
            self.apply_action(action, host);
        }
    }

    /// Dispatch a single action object, gated by granted capabilities.
    fn apply_action(&self, action: &Value, host: &mut dyn Host) {
        let kind = action.get("action").and_then(|v| v.as_str()).unwrap_or("");
        let field = |k: &str| action.get(k).and_then(|v| v.as_str());
        match kind {
            "insert" | "replace_selection" | "replace_line" if self.has_cap("edit") => {
                if let Some(text) = field("text") {
                    apply_edit(kind, host, text);
                }
            }
            "notify" if self.has_cap("ui") => {
                if let Some(msg) = field("message") {
                    host.notify(msg.to_string());
                }
            }
            "open" if self.has_cap("fs:read") => {
                if let Some(path) = field("path") {
                    host.open_path(Path::new(path));
                }
            }
            "run" => {
                if let Some(cmd) = field("command") {
                    host.execute(cmd);
                }
            }
            _ => {}
        }
    }
}

/// The three `edit`-gated text mutations (mirrors the Rhai tier's `apply_edit`).
pub(super) fn apply_edit(kind: &str, host: &mut dyn Host, text: &str) {
    match kind {
        "insert" => runtime::insert_at_cursor(host, text),
        "replace_selection" => runtime::replace_selection(host, text),
        "replace_line" => runtime::replace_line(host, text),
        _ => {}
    }
}
