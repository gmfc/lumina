//! The contribution registry: owns plugin instances, aggregates their declarative
//! contributions, and routes commands/events/panel interactions to the owning plugin.
//!
//! `id → title → handler` plus keybindings, panels, and event subscriptions — the seed of
//! the plugin system (plan §6A, Phase 7). Native built-ins and external guests register
//! the *same* way; the self-hosting test proves it.

use std::collections::HashMap;

use crate::contribution::{
    CommandSpec, Contributions, KeybindingSpec, LanguageSpec, PanelSpec, StatusItemSpec,
};
use crate::event::Event;
use crate::host::Host;

/// A unit of functionality. Native plugins implement this directly; external (Wasm)
/// plugins are wrapped in a host-side adapter that also implements it.
pub trait Plugin {
    /// Stable unique id (e.g. `"explorer"`).
    fn id(&self) -> &str;

    /// Declarative contributions (commands, panels, keybindings, …).
    fn contributions(&self) -> Contributions {
        Contributions::default()
    }

    /// Called once when the plugin is registered.
    fn activate(&mut self, _host: &mut dyn Host) {}

    /// Handle a command this plugin contributed. Return `true` if handled.
    fn run_command(&mut self, _command_id: &str, _host: &mut dyn Host) -> bool {
        false
    }

    /// React to an editor event.
    fn on_event(&mut self, _event: &Event, _host: &mut dyn Host) {}

    /// (Re)render one of this plugin's panels into the host.
    fn render_panel(&mut self, _panel_id: &str, _host: &mut dyn Host) {}

    /// A row in one of this plugin's panels was activated (clicked / Enter).
    fn on_panel_activate(&mut self, _panel_id: &str, _payload: &str, _host: &mut dyn Host) {}

    /// Pre-empt a raw key before chord resolution. A modal layer (vim) or a focused terminal
    /// returns `true` to consume the key; the default returns `false` so an ordinary plugin is
    /// never offered raw input. Powerful — a plugin that returns `true` swallows the keystroke —
    /// so for external guests this is gated behind a `keys:raw` capability.
    fn capture_key(&mut self, _key: crate::input::Key, _host: &mut dyn Host) -> bool {
        false
    }

    /// Handle a raw key while this plugin's [`crate::overlay::Prompt`] (`prompt_id`) is up. The
    /// app routes keys here instead of chord resolution while a prompt owned by this plugin is
    /// active. Return `true` if handled. Default `false`.
    fn on_prompt_key(
        &mut self,
        _prompt_id: &str,
        _key: crate::input::Key,
        _host: &mut dyn Host,
    ) -> bool {
        false
    }

    /// A row of this plugin's picker (`token`) was activated. `item_id` is the chosen row's id.
    /// The plugin acts through `host` (e.g. `execute` a command or `open_path` a file).
    fn on_picker_activate(&mut self, _token: &str, _item_id: &str, _host: &mut dyn Host) {}
}

/// The registry. Owns the live plugins and the aggregated contribution tables.
pub struct Registry {
    plugins: Vec<Box<dyn Plugin>>,
    commands: Vec<CommandSpec>,
    panels: Vec<PanelSpec>,
    status_items: Vec<StatusItemSpec>,
    keybindings: Vec<KeybindingSpec>,
    languages: Vec<LanguageSpec>,
    /// command id -> owning plugin index.
    command_owner: HashMap<String, usize>,
    /// panel id -> owning plugin index.
    panel_owner: HashMap<String, usize>,
}

impl Registry {
    /// Build a registry from a set of plugins, aggregating their contributions.
    pub fn with_plugins<I>(plugins: I) -> Registry
    where
        I: IntoIterator<Item = Box<dyn Plugin>>,
    {
        let mut reg = Registry {
            plugins: Vec::new(),
            commands: Vec::new(),
            panels: Vec::new(),
            status_items: Vec::new(),
            keybindings: Vec::new(),
            languages: Vec::new(),
            command_owner: HashMap::new(),
            panel_owner: HashMap::new(),
        };
        for plugin in plugins {
            reg.add(plugin);
        }
        reg
    }

    fn add(&mut self, plugin: Box<dyn Plugin>) {
        let idx = self.plugins.len();
        let contrib = plugin.contributions();
        for c in contrib.commands {
            self.command_owner.insert(c.id.clone(), idx);
            self.commands.push(c);
        }
        for p in contrib.panels {
            self.panel_owner.insert(p.id.clone(), idx);
            self.panels.push(p);
        }
        self.status_items.extend(contrib.status_items);
        self.keybindings.extend(contrib.keybindings);
        self.languages.extend(contrib.languages);
        self.plugins.push(plugin);
    }

    /// Activate every plugin (called once after wiring the host).
    pub fn activate_all(&mut self, host: &mut dyn Host) {
        for p in &mut self.plugins {
            p.activate(host);
        }
    }

    // --- contribution accessors ------------------------------------------------

    /// All contributed command ids (owned, so callers can compare against `&str` and
    /// `&String` alike — the self-hosting test relies on this).
    pub fn command_ids(&self) -> impl Iterator<Item = String> + '_ {
        self.commands.iter().map(|c| c.id.clone())
    }

    /// The ids of every loaded plugin (built-in and external), in load order.
    pub fn plugin_ids(&self) -> impl Iterator<Item = &str> + '_ {
        self.plugins.iter().map(|p| p.id())
    }

    /// All contributed panel ids.
    pub fn panel_ids(&self) -> impl Iterator<Item = String> + '_ {
        self.panels.iter().map(|p| p.id.clone())
    }

    pub fn commands(&self) -> &[CommandSpec] {
        &self.commands
    }

    pub fn panels(&self) -> &[PanelSpec] {
        &self.panels
    }

    pub fn status_items(&self) -> &[StatusItemSpec] {
        &self.status_items
    }

    pub fn keybindings(&self) -> &[KeybindingSpec] {
        &self.keybindings
    }

    pub fn languages(&self) -> &[LanguageSpec] {
        &self.languages
    }

    /// Look up a command's title (for the palette).
    pub fn command_title(&self, id: &str) -> Option<&str> {
        self.commands
            .iter()
            .find(|c| c.id == id)
            .map(|c| c.title.as_str())
    }

    // --- routing ---------------------------------------------------------------

    /// Dispatch a command to its owning plugin. Returns `true` if a plugin handled it.
    pub fn dispatch_command(&mut self, command_id: &str, host: &mut dyn Host) -> bool {
        if let Some(&idx) = self.command_owner.get(command_id) {
            return self.plugins[idx].run_command(command_id, host);
        }
        false
    }

    /// Broadcast an event to every plugin.
    pub fn broadcast(&mut self, event: &Event, host: &mut dyn Host) {
        for p in &mut self.plugins {
            p.on_event(event, host);
        }
    }

    /// Ask a panel's owning plugin to (re)render it.
    pub fn render_panel(&mut self, panel_id: &str, host: &mut dyn Host) {
        if let Some(&idx) = self.panel_owner.get(panel_id) {
            self.plugins[idx].render_panel(panel_id, host);
        }
    }

    /// Deliver a panel row activation to the owning plugin.
    pub fn activate_panel_row(&mut self, panel_id: &str, payload: &str, host: &mut dyn Host) {
        if let Some(&idx) = self.panel_owner.get(panel_id) {
            self.plugins[idx].on_panel_activate(panel_id, payload, host);
        }
    }

    /// Offer a raw key to plugins that intercept input (vim, a focused terminal) before chord
    /// resolution, in load order. Returns `true` as soon as one consumes it. A no-op until a
    /// plugin overrides [`Plugin::capture_key`], so wiring this into the app's key path is
    /// behavior-preserving on its own.
    pub fn capture_key(&mut self, key: crate::input::Key, host: &mut dyn Host) -> bool {
        for p in &mut self.plugins {
            if p.capture_key(key, host) {
                return true;
            }
        }
        false
    }

    /// Route a key to the plugin that owns the active prompt (`owner` = its id). Returns `true`
    /// if that plugin handled it. Used by the app while `EditorState.prompt` is `Some`.
    pub fn dispatch_prompt_key(
        &mut self,
        owner: &str,
        prompt_id: &str,
        key: crate::input::Key,
        host: &mut dyn Host,
    ) -> bool {
        if let Some(p) = self.plugins.iter_mut().find(|p| p.id() == owner) {
            return p.on_prompt_key(prompt_id, key, host);
        }
        false
    }

    /// Route a picker-row activation to the plugin that owns the picker (`owner` = its id).
    pub fn activate_picker(
        &mut self,
        owner: &str,
        token: &str,
        item_id: &str,
        host: &mut dyn Host,
    ) {
        if let Some(p) = self.plugins.iter_mut().find(|p| p.id() == owner) {
            p.on_picker_activate(token, item_id, host);
        }
    }

    /// Number of registered plugins.
    pub fn plugin_count(&self) -> usize {
        self.plugins.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contribution::{Contributions, PanelLocation};

    struct Dummy;
    impl Plugin for Dummy {
        fn id(&self) -> &str {
            "dummy"
        }
        fn contributions(&self) -> Contributions {
            Contributions::builder()
                .command("dummy.hello", "Hello")
                .panel("dummy.panel", "Panel", PanelLocation::Sidebar)
                .build()
        }
    }

    #[test]
    fn aggregates_ids() {
        let reg = Registry::with_plugins([Box::new(Dummy) as Box<dyn Plugin>]);
        assert!(reg.command_ids().any(|id| id == "dummy.hello"));
        assert!(reg.panel_ids().any(|id| id == "dummy.panel"));
        assert_eq!(reg.plugin_count(), 1);
    }

    /// A minimal in-memory [`Host`] for routing tests that need no real editor state.
    struct NoopHost {
        ws: editor_core::Workspace,
    }
    impl NoopHost {
        fn new() -> NoopHost {
            NoopHost {
                ws: editor_core::Workspace::new(std::path::PathBuf::from(".")),
            }
        }
    }
    impl Host for NoopHost {
        fn workspace(&self) -> &editor_core::Workspace {
            &self.ws
        }
        fn apply_transaction(&mut self, _doc: editor_core::DocId, _txn: editor_core::Transaction) {}
        fn set_selections(&mut self, _doc: editor_core::DocId, _sel: editor_core::Selections) {}
        fn open_path(&mut self, _path: &std::path::Path) {}
        fn read_dir(&self, _path: &std::path::Path) -> Vec<crate::host::DirEntry> {
            Vec::new()
        }
        fn set_panel(&mut self, _panel_id: &str, _content: crate::host::PanelContent) {}
        fn set_status(&mut self, _item_id: &str, _text: String) {}
        fn notify(&mut self, _message: String) {}
        fn execute(&mut self, _command_id: &str) {}
    }

    /// Consumes exactly one chord (`ctrl+d`), recording that it saw it.
    struct Capturer {
        saw: bool,
    }
    impl Plugin for Capturer {
        fn id(&self) -> &str {
            "capturer"
        }
        fn capture_key(&mut self, key: crate::input::Key, _host: &mut dyn Host) -> bool {
            self.saw = true;
            key.ctrl && key.code == crate::input::KeyCode::Char('d')
        }
    }

    #[test]
    fn capture_key_offers_the_key_and_stops_at_the_first_consumer() {
        let mut reg = Registry::with_plugins([
            Box::new(Capturer { saw: false }) as Box<dyn Plugin>,
            Box::new(Dummy),
        ]);
        let mut host = NoopHost::new();
        // A plain plugin never captures (Dummy uses the default false impl).
        let mut only_dummy = Registry::with_plugins([Box::new(Dummy) as Box<dyn Plugin>]);
        assert!(!only_dummy.capture_key(crate::input::Key::char('d').with_ctrl(), &mut host));

        // The capturer consumes ctrl+d …
        assert!(reg.capture_key(crate::input::Key::char('d').with_ctrl(), &mut host));
        // … but declines an unrelated key, so it falls through (returns false).
        assert!(!reg.capture_key(crate::input::Key::char('x'), &mut host));
    }
}
