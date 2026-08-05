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
//! **actions**; the host applies only the ones the manifest was granted (`edit`, `ui`, `fs:read`,
//! `commands:run`). Rhai itself exposes no filesystem or network, and operation/among limits bound
//! runaway loops — the guest physically cannot do what we don't wire up.
//!
//! `commands:run` is transitively the broadest grant: it lets a guest `execute` **any** registered
//! command id, including builtin commands that open terminals (`terminal_open`), issue LSP requests
//! (`lsp_request`) / apply workspace edits (`apply_workspace_edit`), or spawn background jobs
//! (`spawn_job`). Those Host ports are not otherwise reachable by a guest, so they need no separate
//! grant — but `commands:run` should be granted deliberately.

use std::path::Path;

use rhai::{Array, Dynamic, Engine, Scope, AST};

use crate::contribution::{Contributions, PanelLocation};
use crate::registry::Plugin;
use crate::Host;

mod actions;
mod dispatch;
mod manifest;

pub(crate) use actions::{insert_at_cursor, replace_line, replace_selection};
pub(crate) use manifest::Manifest;
use manifest::{lines_to_panel, lines_to_viewer};

/// Bytes of a viewed file handed to a script viewer. Well under Rhai's 2 MB string cap, and a
/// hard bound on the work an external plugin can be made to do by opening a big file.
const SCRIPT_VIEWER_BYTES: usize = 1024 * 1024;

/// A loaded external plugin backed by a Rhai script.
pub(crate) struct ScriptPlugin {
    id: String,
    contributions: Contributions,
    capabilities: Vec<String>,
    engine: Engine,
    ast: AST,
    command_ids: Vec<String>,
    panel_ids: Vec<String>,
    viewer_ids: Vec<String>,
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
    // A missing `plugin.toml` means "this dir isn't a plugin" — stay quiet. But a *malformed*
    // manifest or an un-compilable script is an authoring bug: surfacing it via `eprintln!`
    // (per §5) keeps the plugin from silently vanishing, mirroring the run path's `host.notify`.
    let manifest_src = std::fs::read_to_string(dir.join("plugin.toml")).ok()?;
    let manifest: Manifest = match toml::from_str(&manifest_src) {
        Ok(manifest) => manifest,
        Err(e) => {
            eprintln!("[plugin] {}: invalid plugin.toml: {e}", dir.display());
            return None;
        }
    };
    if manifest.runtime.as_deref() == Some("wasm") {
        return None; // handled by the wasm tier
    }
    let entry = manifest.entry.clone().unwrap_or_else(|| "main.rhai".into());
    let script = std::fs::read_to_string(dir.join(&entry)).ok()?;

    let mut engine = Engine::new();
    // Sandbox limits (plan §11: fuel/epoch-style runaway bounds).
    //
    // Expression depth is pinned rather than left to Rhai's default, which is *halved in debug
    // builds* (16 inside a function vs 32 in release). Leaving it implicit means a plugin that
    // compiles against a released `lmn` fails to load under `cargo run` — a difference no plugin
    // author could diagnose. These are Rhai's release defaults, applied to every profile.
    engine.set_max_expr_depths(64, 32);
    engine.set_max_operations(2_000_000);
    engine.set_max_call_levels(64);
    engine.set_max_string_size(2_000_000);
    engine.set_max_array_size(200_000);
    engine.set_max_map_size(200_000);
    let ast = match engine.compile(&script) {
        Ok(ast) => ast,
        Err(e) => {
            eprintln!("[plugin] {}: Rhai compile error: {e}", dir.display());
            return None;
        }
    };

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
    let mut viewer_ids = Vec::new();
    for v in &manifest.viewers {
        let exts: Vec<&str> = v.extensions.iter().map(String::as_str).collect();
        builder = builder.viewer(v.id.clone(), v.title.clone(), &exts);
        viewer_ids.push(v.id.clone());
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
        viewer_ids,
    })
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

    /// Render a manifest-declared viewer by calling the script's `render_viewer(id, ctx)`, whose
    /// returned array of strings becomes the tab's rows.
    ///
    /// Two capability gates, matching the action dispatcher's: publishing needs `ui`, and the
    /// file's bytes are handed over only with `fs:read` — a script with neither gets the path
    /// string and nothing else. Rhai cannot reach the filesystem itself, so this really is the
    /// only way for a guest viewer to see file content.
    fn render_viewer(
        &mut self,
        viewer_id: &str,
        doc: editor_core::DocId,
        path: &std::path::Path,
        host: &mut dyn Host,
    ) {
        if !self.viewer_ids.iter().any(|v| v == viewer_id) || !self.has_cap("ui") {
            return;
        }
        let mut ctx = Self::build_ctx(host);
        ctx.insert("path".into(), path.to_string_lossy().into_owned().into());
        if self.has_cap("fs:read") {
            let text = std::fs::read(path)
                .map(|bytes| {
                    let end = bytes.len().min(SCRIPT_VIEWER_BYTES);
                    String::from_utf8_lossy(&bytes[..end]).into_owned()
                })
                .unwrap_or_default();
            ctx.insert("text".into(), text.into());
        }
        let mut scope = Scope::new();
        let result: Result<Dynamic, _> = self.engine.call_fn(
            &mut scope,
            &self.ast,
            "render_viewer",
            (viewer_id.to_string(), ctx),
        );
        match result {
            Ok(value) => {
                if let Some(lines) = value.try_cast::<Array>() {
                    host.set_viewer_content(doc, lines_to_viewer(lines));
                }
            }
            Err(_) => host.set_viewer_content(
                doc,
                crate::viewer::ViewerContent::status_only(format!("[{}] viewer error", self.id)),
            ),
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
