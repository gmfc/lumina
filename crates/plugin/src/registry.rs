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
}
