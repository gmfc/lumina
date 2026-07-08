//! WebAssembly external plugin substrate (plan §6A, §11 — the recommended third-party tier).
//!
//! A `.wasm` (or `.wat`) guest dropped in a plugins dir registers through the **same**
//! [`crate::Registry`] / [`crate::Host`] the built-ins and Rhai guests use — no privileged
//! back doors. It is sandboxed and **deny-by-default**: the guest has no imports at all, so it
//! physically cannot touch the filesystem, network, or clock. It communicates only by
//! returning a JSON list of *actions*; the host applies only the ones the manifest was granted
//! (`edit`, `ui`, `fs:read`). A per-call **fuel** budget bounds runaway loops without killing
//! the editor.
//!
//! ## Engine choice
//! The plan recommends WebAssembly as the strong-sandbox substrate (wasmtime/extism). We use
//! [`wasmi`] — a pure-Rust interpreter with the same guarantees (deny-by-default: no host
//! imports are wired; runaway-isolated via fuel metering) — because it compiles in seconds
//! with no C toolchain, keeping CI green on three OSes (the same constraint that shaped the
//! Rhai tier). The `Plugin`/`Host` contract is engine-agnostic; a wasmtime adapter would
//! implement the identical trait.
//!
//! ## ABI
//! The guest exports linear `memory` and `on_command(ctx_ptr, ctx_len) -> i64` (and optionally
//! `render_panel`, `on_activate`), returning a packed `(out_ptr << 32 | out_len)` that points
//! at UTF-8 JSON in its memory. If the guest exports `alloc(len) -> ptr`, the host writes the
//! JSON context there; otherwise it passes `(0, 0)`.

use std::path::Path;

use serde_json::{json, Value};
use wasmi::{Config, Engine, Linker, Module, Store};

use crate::contribution::{Contributions, PanelLocation};
use crate::host::{PanelContent, PanelLine, Span};
use crate::registry::Plugin;
use crate::runtime::{self, Manifest};
use crate::Host;

#[cfg(test)]
mod tests;

/// Per-call fuel budget — generous for real work, finite so an infinite loop can't hang the UI.
const FUEL: u64 = 50_000_000;

/// A loaded external plugin backed by a WebAssembly module.
pub struct WasmPlugin {
    id: String,
    contributions: Contributions,
    capabilities: Vec<String>,
    command_ids: Vec<String>,
    panel_ids: Vec<String>,
    engine: Engine,
    module: Module,
}

/// Load a WebAssembly plugin from `dir`, or `None` if it isn't one / fails to compile.
pub fn load_one(dir: &Path) -> Option<Box<dyn Plugin>> {
    let src = std::fs::read_to_string(dir.join("plugin.toml")).ok()?;
    let manifest: Manifest = toml::from_str(&src).ok()?;
    if manifest.runtime.as_deref() != Some("wasm") {
        return None;
    }
    let entry = manifest.entry.clone().unwrap_or_else(|| "main.wasm".into());
    let raw = std::fs::read(dir.join(&entry)).ok()?;
    // Accept a `.wat` text module for readable authoring; assemble it to a binary module.
    let wasm = if entry.ends_with(".wat") {
        wat::parse_bytes(&raw).ok()?.into_owned()
    } else {
        raw
    };

    let mut config = Config::default();
    config.consume_fuel(true);
    let engine = Engine::new(&config);
    let module = Module::new(&engine, &wasm[..]).ok()?;

    let mut builder = Contributions::builder();
    let mut command_ids = Vec::new();
    let mut panel_ids = Vec::new();
    for c in &manifest.commands {
        builder = builder.command(c.id.clone(), c.title.clone());
        command_ids.push(c.id.clone());
    }
    for p in &manifest.panels {
        let loc = match p.location.as_str() {
            "bottom" => PanelLocation::Bottom,
            _ => PanelLocation::Sidebar,
        };
        builder = builder.panel(p.id.clone(), p.title.clone(), loc);
        panel_ids.push(p.id.clone());
    }
    for k in &manifest.keybindings {
        builder = builder.keybinding(k.chord.clone(), k.command.clone());
    }
    let _ = manifest.name;

    Some(Box::new(WasmPlugin {
        id: manifest.id,
        contributions: builder.build(),
        capabilities: manifest.capabilities,
        command_ids,
        panel_ids,
        engine,
        module,
    }))
}

impl Plugin for WasmPlugin {
    fn id(&self) -> &str {
        &self.id
    }

    fn contributions(&self) -> Contributions {
        self.contributions.clone()
    }

    fn run_command(&mut self, command_id: &str, host: &mut dyn Host) -> bool {
        if !self.command_ids.iter().any(|c| c == command_id) {
            return false;
        }
        let ctx = Self::build_ctx(host);
        match self.call("on_command", &ctx) {
            Some(actions) => self.apply_actions(&actions, host),
            None => host.notify(format!("[{}] wasm command error", self.id)),
        }
        true
    }

    fn render_panel(&mut self, panel_id: &str, host: &mut dyn Host) {
        if !self.panel_ids.iter().any(|p| p == panel_id) || !self.has_cap("ui") {
            return;
        }
        let ctx = Self::build_ctx(host);
        if let Some(Value::Array(lines)) = self.call("render_panel", &ctx) {
            let lines = lines
                .iter()
                .filter_map(|l| l.as_str())
                .map(|s| PanelLine::new(vec![Span::plain(s)]))
                .collect();
            host.set_panel(panel_id, PanelContent { lines, selected: 0 });
        }
    }
}

// --- execution internals: context construction, guest calls, action dispatch ---------------

impl WasmPlugin {
    fn has_cap(&self, cap: &str) -> bool {
        self.capabilities.iter().any(|c| c == cap)
    }

    /// Read-only context handed to the guest (same shape as the Rhai tier's).
    fn build_ctx(host: &dyn Host) -> Value {
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
    fn call(&self, func: &str, ctx: &Value) -> Option<Value> {
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
    fn apply_actions(&self, actions: &Value, host: &mut dyn Host) {
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
fn apply_edit(kind: &str, host: &mut dyn Host, text: &str) {
    match kind {
        "insert" => runtime::insert_at_cursor(host, text),
        "replace_selection" => runtime::replace_selection(host, text),
        "replace_line" => runtime::replace_line(host, text),
        _ => {}
    }
}
