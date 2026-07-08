//! Manifest schema (`plugin.toml`) and small helpers for turning script values into panels.

use rhai::{Array, Map};
use serde::Deserialize;

use crate::host::{PanelContent, PanelLine, Span};

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

pub(super) fn str_field(map: &Map, key: &str) -> Option<String> {
    map.get(key).and_then(|d| d.clone().into_string().ok())
}

pub(super) fn panel_from_map(map: &Map) -> PanelContent {
    let lines = map
        .get("lines")
        .and_then(|d| d.clone().try_cast::<Array>())
        .unwrap_or_default();
    lines_to_panel(lines)
}

pub(super) fn lines_to_panel(lines: Array) -> PanelContent {
    let lines = lines
        .into_iter()
        .filter_map(|d| d.into_string().ok())
        .map(|s| PanelLine::new(vec![Span::plain(s)]))
        .collect();
    PanelContent { lines, selected: 0 }
}
