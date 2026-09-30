//! Language registry: language id → grammar + highlights query.
//!
//! # User-loadable highlight overrides
//!
//! Built-in grammars are compiled into the binary. Users can **override the highlights query**
//! for a wired language without rebuilding: drop a `highlights.scm` at
//! `~/.config/lumina/grammars/<lang_id>/highlights.scm` (or under any directory listed in
//! config `grammar_dirs`). The grammar / parser itself stays the built-in one — only the
//! query text is replaced. This path is intentionally query-only (no `dlopen`, no unsafe).
//!
//! Call [`load_user_queries`] once at highlighter construction (or config reload) and pass the
//! map into [`lang_config`] / [`DocHighlighter::new_with_queries`].

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use tree_sitter::Language;

/// Scan `dirs` for `<dir>/<lang_id>/highlights.scm` files and return `lang_id → query source`.
/// Later directories win when the same `lang_id` appears more than once. Missing / unreadable
/// files are skipped silently (a typo in one override must not disable highlighting).
pub fn load_user_queries(dirs: &[PathBuf]) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for dir in dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let Some(lang_id) = path.file_name().and_then(|n| n.to_str()) else {
                continue;
            };
            let query_path = path.join("highlights.scm");
            if let Ok(src) = std::fs::read_to_string(&query_path) {
                if !src.trim().is_empty() {
                    out.insert(lang_id.to_string(), src);
                }
            }
        }
    }
    out
}

/// Language id → grammar + highlights query. Returns `None` for unsupported languages.
///
/// When `user_queries` contains an entry for `id`, that query text replaces the built-in one.
///
/// Grammar crates are decoupled from the tree-sitter runtime version (they only provide a
/// `LanguageFn` + query text), so new languages are a table entry, not a version bump.
pub fn lang_config(id: &str, user_queries: &HashMap<String, String>) -> Option<(Language, String)> {
    let (lang, builtin): (Language, String) = match id {
        "rust" => (
            tree_sitter_rust::LANGUAGE.into(),
            tree_sitter_rust::HIGHLIGHTS_QUERY.to_string(),
        ),
        // A compact, version-independent JSON highlights query.
        "json" => (
            tree_sitter_json::LANGUAGE.into(),
            r#"
            (pair key: (string) @property)
            (string) @string
            (number) @number
            [(true) (false)] @constant.builtin
            (null) @constant.builtin
            (comment) @comment
            ["," ":"] @punctuation.delimiter
            ["{" "}" "[" "]"] @punctuation.bracket
            "#
            .to_string(),
        ),
        "python" => (
            tree_sitter_python::LANGUAGE.into(),
            tree_sitter_python::HIGHLIGHTS_QUERY.to_string(),
        ),
        "javascript" => (
            tree_sitter_javascript::LANGUAGE.into(),
            tree_sitter_javascript::HIGHLIGHT_QUERY.to_string(),
        ),
        // TypeScript's grammar is a JS superset; its highlights build on the JS query.
        "typescript" => (
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
            format!(
                "{}\n{}",
                tree_sitter_javascript::HIGHLIGHT_QUERY,
                tree_sitter_typescript::HIGHLIGHTS_QUERY
            ),
        ),
        "c" => (
            tree_sitter_c::LANGUAGE.into(),
            tree_sitter_c::HIGHLIGHT_QUERY.to_string(),
        ),
        "go" => (
            tree_sitter_go::LANGUAGE.into(),
            tree_sitter_go::HIGHLIGHTS_QUERY.to_string(),
        ),
        "toml" => (
            tree_sitter_toml_ng::LANGUAGE.into(),
            tree_sitter_toml_ng::HIGHLIGHTS_QUERY.to_string(),
        ),
        // Markdown's grammar is split block/inline; we highlight the block layer.
        "markdown" => (
            tree_sitter_md::LANGUAGE.into(),
            tree_sitter_md::HIGHLIGHT_QUERY_BLOCK.to_string(),
        ),
        "html" => (
            tree_sitter_html::LANGUAGE.into(),
            tree_sitter_html::HIGHLIGHTS_QUERY.to_string(),
        ),
        "css" => (
            tree_sitter_css::LANGUAGE.into(),
            tree_sitter_css::HIGHLIGHTS_QUERY.to_string(),
        ),
        "java" => (
            tree_sitter_java::LANGUAGE.into(),
            tree_sitter_java::HIGHLIGHTS_QUERY.to_string(),
        ),
        "ruby" => (
            tree_sitter_ruby::LANGUAGE.into(),
            tree_sitter_ruby::HIGHLIGHTS_QUERY.to_string(),
        ),
        "bash" => (
            tree_sitter_bash::LANGUAGE.into(),
            tree_sitter_bash::HIGHLIGHT_QUERY.to_string(),
        ),
        "yaml" => (
            tree_sitter_yaml::LANGUAGE.into(),
            tree_sitter_yaml::HIGHLIGHTS_QUERY.to_string(),
        ),
        _ => return None,
    };
    let query = user_queries.get(id).cloned().unwrap_or(builtin);
    Some((lang, query))
}

/// True if a language id has a grammar wired in.
pub fn is_supported(lang_id: &str) -> bool {
    lang_config(lang_id, &HashMap::new()).is_some()
}

/// Convenience: load user queries from a single directory if it exists.
pub fn load_user_queries_from(dir: &Path) -> HashMap<String, String> {
    load_user_queries(&[dir.to_path_buf()])
}
