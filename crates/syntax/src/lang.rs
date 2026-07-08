//! Language registry: language id → grammar + highlights query.

use tree_sitter::Language;

/// Language id → grammar + highlights query. Returns `None` for unsupported languages.
///
/// Grammar crates are decoupled from the tree-sitter runtime version (they only provide a
/// `LanguageFn` + query text), so new languages are a table entry, not a version bump.
pub(crate) fn lang_config(id: &str) -> Option<(Language, String)> {
    let (lang, query): (Language, String) = match id {
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
        _ => return None,
    };
    Some((lang, query))
}

/// True if a language id has a grammar wired in.
pub fn is_supported(lang_id: &str) -> bool {
    lang_config(lang_id).is_some()
}
