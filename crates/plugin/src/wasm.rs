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

use serde_json::Value;
use wasmi::{Config, Engine, Module};

use crate::contribution::{Contributions, PanelLocation};
use crate::host::{PanelContent, PanelLine, Span};
use crate::registry::Plugin;
use crate::runtime::Manifest;
use crate::Host;

mod engine;
#[cfg(test)]
mod tests;

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
