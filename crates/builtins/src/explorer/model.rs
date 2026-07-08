//! The explorer's row model and file-icon mapping.

use std::path::{Path, PathBuf};

/// One visible row of the flattened tree (plan §6: keep a flat `Vec` for O(1) hit-testing).
pub(super) struct Row {
    pub(super) path: PathBuf,
    pub(super) is_dir: bool,
    pub(super) depth: usize,
    pub(super) expanded: bool,
}

/// A Nerd Font glyph for a file, chosen by extension (requires a patched font; opt-in).
pub(super) fn file_glyph(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()) {
        Some("rs") => "\u{e7a8}",                   //
        Some("py") => "\u{e606}",                   //
        Some("js" | "mjs" | "cjs") => "\u{e781}",   //
        Some("ts" | "tsx") => "\u{e628}",           //
        Some("json") => "\u{e60b}",                 //
        Some("toml" | "ini" | "cfg") => "\u{e615}", //
        Some("md" | "markdown") => "\u{e73e}",      //
        Some("c" | "h") => "\u{e61e}",              //
        Some("go") => "\u{e627}",                   //
        Some("lock") => "\u{f023}",                 //
        _ => "\u{f15b}",                            //
    }
}
