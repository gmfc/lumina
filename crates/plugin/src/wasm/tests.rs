use super::apply_edit;
use super::*;
use std::collections::HashMap;
use std::path::PathBuf;

use serde_json::json;

use editor_core::{DocId, Document, Selections, Transaction, Workspace};

use crate::host::DirEntry;

/// A minimal in-memory [`Host`] for exercising a plugin without the full app.
struct TestHost {
    ws: Workspace,
    panels: HashMap<String, PanelContent>,
    executed: Vec<String>,
}

impl TestHost {
    fn with_doc(text: &str) -> (TestHost, DocId) {
        let mut ws = Workspace::new(PathBuf::from("."));
        let id = ws.open_document(Document::from_str(text));
        ws.documents.get_mut(id).unwrap().set_caret(0);
        (
            TestHost {
                ws,
                panels: HashMap::new(),
                executed: Vec::new(),
            },
            id,
        )
    }
    fn text(&self, id: DocId) -> String {
        self.ws.documents.get(id).unwrap().to_string()
    }
}

impl Host for TestHost {
    fn workspace(&self) -> &Workspace {
        &self.ws
    }
    fn apply_transaction(&mut self, doc: DocId, txn: Transaction) {
        if let Some(d) = self.ws.documents.get_mut(doc) {
            txn.apply(d);
        }
    }
    fn set_selections(&mut self, doc: DocId, selections: Selections) {
        if let Some(d) = self.ws.documents.get_mut(doc) {
            d.selections = selections;
        }
    }
    fn open_path(&mut self, _path: &Path) {}
    fn read_dir(&self, _path: &Path) -> Vec<DirEntry> {
        Vec::new()
    }
    fn set_panel(&mut self, panel_id: &str, content: PanelContent) {
        self.panels.insert(panel_id.to_string(), content);
    }
    fn set_status(&mut self, _item_id: &str, _text: String) {}
    fn notify(&mut self, _message: String) {}
    fn execute(&mut self, command_id: &str) {
        self.executed.push(command_id.to_string());
    }
}

fn example_plugin_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins/wasm-hello")
}

fn empty_module() -> (Engine, Module) {
    let mut config = Config::default();
    config.consume_fuel(true);
    let engine = Engine::new(&config);
    let wasm = wat::parse_str("(module)").unwrap();
    let module = Module::new(&engine, &wasm[..]).unwrap();
    (engine, module)
}

#[test]
fn wasm_plugin_loads_and_contributes() {
    let plugin = load_one(&example_plugin_dir()).expect("example wasm plugin loads");
    let c = plugin.contributions();
    assert!(c.commands.iter().any(|c| c.id == "wasm-hello.insert"));
    assert!(c.panels.iter().any(|p| p.id == "wasm-hello.panel"));
}

#[test]
fn wasm_command_edits_buffer_through_granted_capability() {
    let mut plugin = load_one(&example_plugin_dir()).unwrap();
    let (mut host, id) = TestHost::with_doc("fn main() {}");
    assert!(plugin.run_command("wasm-hello.insert", &mut host));
    assert!(
        host.text(id).starts_with("// hello from wasm\n"),
        "guest edit not applied: {:?}",
        host.text(id)
    );
}

#[test]
fn wasm_panel_renders_lines() {
    let mut plugin = load_one(&example_plugin_dir()).unwrap();
    let (mut host, _id) = TestHost::with_doc("x");
    plugin.render_panel("wasm-hello.panel", &mut host);
    let panel = host.panels.get("wasm-hello.panel").expect("panel set");
    assert_eq!(panel.lines.len(), 2);
}

#[test]
fn ungranted_capability_is_denied() {
    // A plugin with no capabilities must not be able to edit, even if it emits the action.
    let (engine, module) = empty_module();
    let denied = WasmPlugin {
        id: "denied".into(),
        contributions: Contributions::default(),
        capabilities: Vec::new(),
        command_ids: Vec::new(),
        panel_ids: Vec::new(),
        engine,
        module,
    };
    let (mut host, id) = TestHost::with_doc("unchanged");
    denied.apply_actions(&json!([{ "action": "insert", "text": "X" }]), &mut host);
    assert_eq!(host.text(id), "unchanged", "deny-by-default was bypassed");
}

#[test]
fn run_action_requires_commands_run_capability() {
    // The `run` action lets a guest invoke arbitrary editor commands (which reach the full
    // registry), so it must be gated like every other action. Without `commands:run` a guest
    // that emits `run` must be ignored; with it, the command reaches the host.
    let (engine, module) = empty_module();
    let denied = WasmPlugin {
        id: "denied".into(),
        contributions: Contributions::default(),
        capabilities: Vec::new(),
        command_ids: Vec::new(),
        panel_ids: Vec::new(),
        engine,
        module,
    };
    let (mut host, _id) = TestHost::with_doc("x");
    denied.apply_actions(
        &json!([{ "action": "run", "command": "file.save" }]),
        &mut host,
    );
    assert!(
        host.executed.is_empty(),
        "ungated `run` escaped the capability sandbox: {:?}",
        host.executed
    );

    let (engine, module) = empty_module();
    let granted = WasmPlugin {
        id: "granted".into(),
        contributions: Contributions::default(),
        capabilities: vec!["commands:run".into()],
        command_ids: Vec::new(),
        panel_ids: Vec::new(),
        engine,
        module,
    };
    let (mut host, _id) = TestHost::with_doc("x");
    granted.apply_actions(
        &json!([{ "action": "run", "command": "file.save" }]),
        &mut host,
    );
    assert_eq!(host.executed, vec!["file.save".to_string()]);
}

#[test]
fn granted_capabilities_dispatch_every_action_kind() {
    let (engine, module) = empty_module();
    let plugin = WasmPlugin {
        id: "multi".into(),
        contributions: Contributions::default(),
        capabilities: vec![
            "edit".into(),
            "ui".into(),
            "fs:read".into(),
            "commands:run".into(),
        ],
        command_ids: Vec::new(),
        panel_ids: Vec::new(),
        engine,
        module,
    };
    let (mut host, id) = TestHost::with_doc("hello world");
    // Every action kind reaches its handler: the edit group flows through apply_edit,
    // and notify / open / run each hit their own arm.
    plugin.apply_actions(
        &json!([
            { "action": "insert", "text": "I" },
            { "action": "replace_selection", "text": "R" },
            { "action": "replace_line", "text": "L" },
            { "action": "notify", "message": "hi" },
            { "action": "open", "path": "/tmp/ignored-by-test-host" },
            { "action": "run", "command": "noop" },
            { "action": "unknown-kind" }
        ]),
        &mut host,
    );
    // A non-array payload is ignored (early return in apply_actions).
    plugin.apply_actions(&json!({ "not": "an array" }), &mut host);
    // apply_edit's fallback arm (a kind outside the three) is a no-op.
    apply_edit("bogus", &mut host, "Z");
    // The edit actions ran, so the buffer changed from its initial contents.
    assert_ne!(host.text(id), "hello world");
}
