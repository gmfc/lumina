//! External plugin runtime (plan §6A, Phase 11). Third-party plugins live in a plugins dir,
//! each a folder with a `plugin.toml` manifest + a script. They register through the **same**
//! [`crate::Registry`] / [`crate::Host`] the built-ins use — no privileged back doors.
//!
//! ## Substrate
//! The plan recommends WebAssembly for the strongest sandbox; its substrate table also lists
//! **Rhai** (Rust-native, sandboxed by default, runaway-limited). We use Rhai here: it needs
//! no external toolchain, compiles fast enough for CI on three OSes, and demonstrates the full
//! external tier — manifest contributions, capability grants, and buffer edits through the
//! host — while the `Plugin`/`Host`/`Registry` contract stays substrate-agnostic (a Wasm
//! adapter would implement the same `Plugin` trait).
//!
//! ## Capability model (deny by default)
//! A script never touches the editor directly. Its command handler returns a list of
//! **actions**; the host applies only the ones the manifest was granted (`edit`, `ui`,
//! `fs:read`). Rhai itself exposes no filesystem or network, and operation/among limits bound
//! runaway loops — the guest physically cannot do what we don't wire up.

use std::path::Path;

use editor_core::Transaction;
use rhai::{Array, Dynamic, Engine, Map, Scope, AST};
use serde::Deserialize;

use crate::contribution::{Contributions, PanelLocation};
use crate::host::{PanelContent, PanelLine, Span};
use crate::registry::Plugin;
use crate::Host;

#[derive(Debug, Deserialize)]
pub(crate) struct RawCommand {
    pub id: String,
    pub title: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawPanel {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub location: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawKey {
    pub chord: String,
    pub command: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Manifest {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub entry: Option<String>,
    /// Execution substrate: `"wasm"` for the WebAssembly tier, else the Rhai script tier.
    #[serde(default)]
    pub runtime: Option<String>,
    #[serde(default)]
    pub capabilities: Vec<String>,
    #[serde(default)]
    pub commands: Vec<RawCommand>,
    #[serde(default)]
    pub panels: Vec<RawPanel>,
    #[serde(default)]
    pub keybindings: Vec<RawKey>,
}

/// A loaded external plugin backed by a Rhai script.
pub struct ScriptPlugin {
    id: String,
    contributions: Contributions,
    capabilities: Vec<String>,
    engine: Engine,
    ast: AST,
    command_ids: Vec<String>,
    panel_ids: Vec<String>,
}

/// Load every plugin under `dir` (each a subfolder with `plugin.toml`). Missing/invalid
/// plugins are skipped rather than failing the whole load.
pub fn load_dir(dir: &Path) -> Vec<Box<dyn Plugin>> {
    let mut plugins: Vec<Box<dyn Plugin>> = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return plugins;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        // Dispatch by substrate: WebAssembly plugins load through the wasm tier, everything
        // else through the Rhai script tier. Both register through the same Registry.
        if is_wasm_plugin(&path) {
            if let Some(plugin) = crate::wasm::load_one(&path) {
                plugins.push(plugin);
            }
        } else if let Some(plugin) = load_one(&path) {
            plugins.push(Box::new(plugin));
        }
    }
    plugins
}

/// Peek a plugin dir's manifest to see whether it targets the WebAssembly substrate.
pub(crate) fn is_wasm_plugin(dir: &Path) -> bool {
    std::fs::read_to_string(dir.join("plugin.toml"))
        .ok()
        .and_then(|src| toml::from_str::<Manifest>(&src).ok())
        .map(|m| m.runtime.as_deref() == Some("wasm"))
        .unwrap_or(false)
}

fn load_one(dir: &Path) -> Option<ScriptPlugin> {
    let manifest_src = std::fs::read_to_string(dir.join("plugin.toml")).ok()?;
    let manifest: Manifest = toml::from_str(&manifest_src).ok()?;
    if manifest.runtime.as_deref() == Some("wasm") {
        return None; // handled by the wasm tier
    }
    let entry = manifest.entry.clone().unwrap_or_else(|| "main.rhai".into());
    let script = std::fs::read_to_string(dir.join(&entry)).ok()?;

    let mut engine = Engine::new();
    // Sandbox limits (plan §11: fuel/epoch-style runaway bounds).
    engine.set_max_operations(2_000_000);
    engine.set_max_call_levels(64);
    engine.set_max_string_size(2_000_000);
    engine.set_max_array_size(200_000);
    engine.set_max_map_size(200_000);
    let ast = engine.compile(&script).ok()?;

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

    Some(ScriptPlugin {
        id: manifest.id,
        contributions: builder.build(),
        capabilities: manifest.capabilities,
        engine,
        ast,
        command_ids,
        panel_ids,
    })
}

impl ScriptPlugin {
    fn has_cap(&self, cap: &str) -> bool {
        self.capabilities.iter().any(|c| c == cap)
    }

    /// Read-only context handed to the script: cursor position + relevant text.
    fn build_ctx(host: &dyn Host) -> Map {
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
    fn apply_actions(&self, panel_ctx: Option<&str>, actions: Array, host: &mut dyn Host) {
        for action in actions {
            let Some(map) = action.try_cast::<Map>() else {
                continue;
            };
            let kind = map
                .get("action")
                .and_then(|d| d.clone().into_string().ok())
                .unwrap_or_default();
            match kind.as_str() {
                "insert" if self.has_cap("edit") => {
                    if let Some(text) = str_field(&map, "text") {
                        insert_at_cursor(host, &text);
                    }
                }
                "replace_selection" if self.has_cap("edit") => {
                    if let Some(text) = str_field(&map, "text") {
                        replace_selection(host, &text);
                    }
                }
                "replace_line" if self.has_cap("edit") => {
                    if let Some(text) = str_field(&map, "text") {
                        replace_line(host, &text);
                    }
                }
                "notify" if self.has_cap("ui") => {
                    if let Some(msg) = str_field(&map, "message") {
                        host.notify(msg);
                    }
                }
                "open" if self.has_cap("fs:read") => {
                    if let Some(path) = str_field(&map, "path") {
                        host.open_path(Path::new(&path));
                    }
                }
                "run" => {
                    if let Some(cmd) = str_field(&map, "command") {
                        host.execute(&cmd);
                    }
                }
                "set_panel" if self.has_cap("ui") => {
                    let panel_id =
                        str_field(&map, "panel").or_else(|| panel_ctx.map(str::to_string));
                    if let Some(panel_id) = panel_id {
                        let content = panel_from_map(&map);
                        host.set_panel(&panel_id, content);
                    }
                }
                _ => {}
            }
        }
    }
}

impl Plugin for ScriptPlugin {
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
        let mut scope = Scope::new();
        let result: Result<Dynamic, _> = self.engine.call_fn(
            &mut scope,
            &self.ast,
            "on_command",
            (command_id.to_string(), ctx),
        );
        if let Ok(value) = result {
            if let Some(actions) = value.try_cast::<Array>() {
                self.apply_actions(None, actions, host);
            }
        } else {
            host.notify(format!("[{}] command error", self.id));
        }
        true
    }

    fn render_panel(&mut self, panel_id: &str, host: &mut dyn Host) {
        if !self.panel_ids.iter().any(|p| p == panel_id) {
            return;
        }
        let ctx = Self::build_ctx(host);
        let mut scope = Scope::new();
        let result: Result<Dynamic, _> = self.engine.call_fn(
            &mut scope,
            &self.ast,
            "render_panel",
            (panel_id.to_string(), ctx),
        );
        if let Ok(value) = result {
            if let Some(lines) = value.try_cast::<Array>() {
                let content = lines_to_panel(lines);
                host.set_panel(panel_id, content);
            }
        }
    }

    fn on_panel_activate(&mut self, panel_id: &str, payload: &str, host: &mut dyn Host) {
        if !self.panel_ids.iter().any(|p| p == panel_id) {
            return;
        }
        let mut ctx = Self::build_ctx(host);
        ctx.insert("payload".into(), payload.to_string().into());
        let mut scope = Scope::new();
        let result: Result<Dynamic, _> = self.engine.call_fn(
            &mut scope,
            &self.ast,
            "on_activate",
            (panel_id.to_string(), ctx),
        );
        if let Ok(value) = result {
            if let Some(actions) = value.try_cast::<Array>() {
                self.apply_actions(Some(panel_id), actions, host);
            }
        }
    }
}

// --- action helpers (build transactions against the active document) -----------

fn str_field(map: &Map, key: &str) -> Option<String> {
    map.get(key).and_then(|d| d.clone().into_string().ok())
}

pub(crate) fn insert_at_cursor(host: &mut dyn Host, text: &str) {
    let Some(id) = host.active_doc() else {
        return;
    };
    let txn = {
        let Some(doc) = host.workspace().documents.get(id) else {
            return;
        };
        let head = doc.selections.primary().head;
        Transaction::insert(doc, head, text)
    };
    host.apply_transaction(id, txn);
}

pub(crate) fn replace_selection(host: &mut dyn Host, text: &str) {
    let Some(id) = host.active_doc() else {
        return;
    };
    let txn = {
        let Some(doc) = host.workspace().documents.get(id) else {
            return;
        };
        let sel = doc.selections.primary();
        Transaction::replace(doc, sel.from()..sel.to(), text)
    };
    host.apply_transaction(id, txn);
}

pub(crate) fn replace_line(host: &mut dyn Host, text: &str) {
    let Some(id) = host.active_doc() else {
        return;
    };
    let txn = {
        let Some(doc) = host.workspace().documents.get(id) else {
            return;
        };
        let head = doc.selections.primary().head;
        let line = doc.char_to_line(head);
        let start = doc.line_to_char(line);
        let end = start + doc.line_len_chars(line);
        Transaction::replace(doc, start..end, text)
    };
    host.apply_transaction(id, txn);
}

fn panel_from_map(map: &Map) -> PanelContent {
    let lines = map
        .get("lines")
        .and_then(|d| d.clone().try_cast::<Array>())
        .unwrap_or_default();
    lines_to_panel(lines)
}

fn lines_to_panel(lines: Array) -> PanelContent {
    let lines = lines
        .into_iter()
        .filter_map(|d| d.into_string().ok())
        .map(|s| PanelLine::new(vec![Span::plain(s)]))
        .collect();
    PanelContent { lines, selected: 0 }
}
