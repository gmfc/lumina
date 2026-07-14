//! LSP navigation responses — go-to-definition, references, document symbols — **as a plugin**
//! (invariant #3).
//!
//! The LSP *requests* are the `lsp` plugin; this owns the *responses* that navigate. It is purely
//! reactive (no commands): the app translates a server response into a primitive
//! [`Event::LspGoto`] (a single target) or [`Event::LspLocations`] (a set to choose from), and this
//! plugin jumps through [`Host::open_location`] — for a set, by opening a generic picker and
//! jumping on activation. The LSP transport and the URI→path resolution stay app-side; the plugin
//! owns only the navigation UX (jump vs. picker) and the picker's target list.

use editor_plugin::{Event, Host, LspLocation, PickerItem, PickerRequest, Plugin};

#[derive(Default)]
pub(crate) struct LspNavPlugin {
    /// Jump targets backing the current picker (references / symbols); a picker row's id is the
    /// index into this.
    locations: Vec<LspLocation>,
}

impl LspNavPlugin {
    const ID: &'static str = "lsp-nav";
    const TOKEN: &'static str = "nav";

    fn jump(host: &mut dyn Host, loc: &LspLocation) {
        host.open_location(std::path::Path::new(&loc.path), loc.line, loc.character);
    }
}

impl Plugin for LspNavPlugin {
    fn id(&self) -> &str {
        Self::ID
    }

    fn on_event(&mut self, event: &Event, host: &mut dyn Host) {
        match event {
            Event::LspGoto(loc) => Self::jump(host, loc),
            Event::LspLocations { title, items } => {
                if items.is_empty() {
                    host.notify(format!("No {title}"));
                    return;
                }
                self.locations = items.iter().map(|i| i.location.clone()).collect();
                let rows = items
                    .iter()
                    .enumerate()
                    .map(|(i, it)| PickerItem::new(i.to_string(), it.label.clone()))
                    .collect();
                host.open_picker(PickerRequest {
                    owner: Self::ID.to_string(),
                    token: Self::TOKEN.to_string(),
                    title: title.clone(),
                    items: rows,
                    commands: Vec::new(),
                    start_in_commands: false,
                });
            }
            _ => {}
        }
    }

    fn on_picker_activate(&mut self, _token: &str, item_id: &str, host: &mut dyn Host) {
        let Some(loc) = item_id
            .parse::<usize>()
            .ok()
            .and_then(|i| self.locations.get(i))
            .cloned()
        else {
            return;
        };
        Self::jump(host, &loc);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use editor_core::{DocId, Selections, Transaction, Workspace};
    use editor_plugin::{LspNavItem, PanelContent};
    use std::path::{Path, PathBuf};

    /// Records the navigation effects the plugin drives (`open_location`, `open_picker`, `notify`);
    /// everything else is a no-op default. The plugin never reads the workspace, so it stays empty.
    #[derive(Default)]
    struct Rec {
        opened: Vec<(String, u32, u32)>,
        picker: Option<PickerRequest>,
        notes: Vec<String>,
    }

    struct MockHost {
        ws: Workspace,
        rec: Rec,
    }

    impl MockHost {
        fn new() -> MockHost {
            MockHost {
                ws: Workspace::new(PathBuf::from(".")),
                rec: Rec::default(),
            }
        }
    }

    impl Host for MockHost {
        fn workspace(&self) -> &Workspace {
            &self.ws
        }
        fn apply_transaction(&mut self, _doc: DocId, _txn: Transaction) {}
        fn set_selections(&mut self, _doc: DocId, _sels: Selections) {}
        fn open_path(&mut self, _path: &Path) {}
        fn open_location(&mut self, path: &Path, line: u32, character: u32) {
            self.rec
                .opened
                .push((path.to_string_lossy().into_owned(), line, character));
        }
        fn open_picker(&mut self, request: PickerRequest) {
            self.rec.picker = Some(request);
        }
        fn set_panel(&mut self, _panel_id: &str, _content: PanelContent) {}
        fn set_status(&mut self, _item_id: &str, _text: String) {}
        fn notify(&mut self, message: String) {
            self.rec.notes.push(message);
        }
        fn execute(&mut self, _command_id: &str) {}
    }

    fn loc(path: &str, line: u32, ch: u32) -> LspLocation {
        LspLocation {
            path: path.into(),
            line,
            character: ch,
        }
    }

    #[test]
    fn goto_jumps_to_the_target() {
        let mut host = MockHost::new();
        LspNavPlugin::default().on_event(&Event::LspGoto(loc("/a.rs", 3, 2)), &mut host);
        assert_eq!(host.rec.opened, vec![("/a.rs".to_string(), 3, 2)]);
    }

    #[test]
    fn locations_open_a_picker_then_activation_jumps() {
        let mut host = MockHost::new();
        let mut plugin = LspNavPlugin::default();
        let items = vec![
            LspNavItem {
                location: loc("/a.rs", 1, 0),
                label: "a.rs:2:1".into(),
            },
            LspNavItem {
                location: loc("/b.rs", 5, 4),
                label: "b.rs:6:5".into(),
            },
        ];
        plugin.on_event(
            &Event::LspLocations {
                title: "References".into(),
                items,
            },
            &mut host,
        );
        let req = host.rec.picker.as_ref().expect("picker opened");
        assert_eq!(req.owner, "lsp-nav");
        assert_eq!(req.title, "References");
        assert_eq!(req.items.len(), 2);
        assert_eq!(req.items[1].label, "b.rs:6:5");
        // Activating the second row jumps to its location.
        plugin.on_picker_activate("nav", "1", &mut host);
        assert_eq!(host.rec.opened, vec![("/b.rs".to_string(), 5, 4)]);
    }

    #[test]
    fn empty_locations_notify_and_open_no_picker() {
        let mut host = MockHost::new();
        LspNavPlugin::default().on_event(
            &Event::LspLocations {
                title: "Symbols".into(),
                items: vec![],
            },
            &mut host,
        );
        assert!(host.rec.picker.is_none());
        assert_eq!(host.rec.notes, vec!["No Symbols".to_string()]);
    }

    #[test]
    fn out_of_range_or_nonnumeric_activation_is_ignored() {
        let mut host = MockHost::new();
        let mut plugin = LspNavPlugin::default();
        plugin.on_picker_activate("nav", "7", &mut host); // nothing stored yet
        plugin.on_picker_activate("nav", "notanumber", &mut host);
        assert!(host.rec.opened.is_empty());
    }
}
