//! LSP semantic tokens, implemented **as a plugin** (invariant #3).
//!
//! Server-truth highlighting layered **over** tree-sitter (§7.1): the app requests full-document
//! tokens (transport + UTF-16↔char mapping stay app-side) and broadcasts the decoded set as
//! [`Event::LspSemanticTokens`]; this plugin maps each token's type/modifiers to a theme scope and
//! publishes them as the `"lsp.semantic"` decoration layer. The layer merges *on top of* the
//! tree-sitter base, so an unmapped token type paints nothing and the syntax colour shows through
//! (we declared `augmentsSyntaxTokens`). Cleared when the layer comes back empty.

use editor_core::DocId;
use editor_plugin::{Decoration, DecorationSet, Event, Host, LspSemanticToken, Plugin};

const LAYER: &str = "lsp.semantic";

/// Map an LSP semantic token type + modifiers to a theme scope (reusing the tree-sitter syntax
/// palette, resolved by `Theme::style_for`'s dotted-scope fallback). Returns an empty string for a
/// type we don't paint — the token is then skipped so tree-sitter shows through. Modifiers refine:
/// `readonly` → constant colour; `defaultLibrary` → a `.builtin` variant.
fn scope_for(token_type: &str, modifiers: &[String]) -> String {
    let base = match token_type {
        "namespace" => "namespace",
        "type" | "class" | "enum" | "interface" | "struct" | "typeParameter" => "type",
        "parameter" => "parameter",
        "variable" => "variable",
        "property" | "enumMember" => "property",
        "function" | "method" | "event" | "decorator" => "function",
        "macro" => "function.macro",
        "keyword" | "modifier" => "keyword",
        "comment" => "comment",
        "string" => "string",
        "number" => "number",
        "regexp" => "string.special",
        "operator" => "operator",
        _ => return String::new(),
    };
    let has = |m: &str| modifiers.iter().any(|x| x == m);
    let mut scope = base.to_string();
    // A readonly binding reads as a constant.
    if has("readonly")
        && matches!(
            token_type,
            "variable" | "property" | "parameter" | "enumMember"
        )
    {
        scope = "constant".to_string();
    }
    // Standard-library symbols get the builtin tint (falls back to the base via dotted-scope
    // resolution when there's no dedicated builtin colour).
    if has("defaultLibrary") {
        scope.push_str(".builtin");
    }
    scope
}

#[derive(Default)]
pub struct SemanticTokensPlugin;

impl SemanticTokensPlugin {
    fn publish(host: &mut dyn Host, doc: DocId, tokens: &[LspSemanticToken]) {
        if tokens.is_empty() {
            host.clear_decorations(doc, LAYER);
            return;
        }
        let spans = tokens
            .iter()
            .filter_map(|t| {
                let scope = scope_for(&t.token_type, &t.modifiers);
                if scope.is_empty() {
                    return None; // unpainted type → let tree-sitter show through
                }
                let start = host.lsp_pos_to_offset(doc, t.line, t.start_char16);
                // `length` is UTF-16 units on the same line, so the end column is start + length.
                let end = host
                    .lsp_pos_to_offset(doc, t.line, t.start_char16 + t.length)
                    .max(start + 1);
                Some(Decoration::new((start, end), scope))
            })
            .collect();
        host.set_decorations(doc, LAYER, DecorationSet::spans(spans));
    }
}

impl Plugin for SemanticTokensPlugin {
    fn id(&self) -> &str {
        "semantic-tokens"
    }

    fn on_event(&mut self, event: &Event, host: &mut dyn Host) {
        if let Event::LspSemanticTokens {
            doc: Some(doc),
            tokens,
        } = event
        {
            Self::publish(host, *doc, tokens);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_types_to_syntax_scopes() {
        assert_eq!(scope_for("function", &[]), "function");
        assert_eq!(scope_for("struct", &[]), "type");
        assert_eq!(scope_for("macro", &[]), "function.macro");
        assert_eq!(scope_for("regexp", &[]), "string.special");
    }

    #[test]
    fn unknown_type_is_unpainted() {
        assert_eq!(scope_for("weird", &[]), "");
    }

    #[test]
    fn modifiers_refine_the_scope() {
        // readonly variable → constant; + defaultLibrary → constant.builtin.
        assert_eq!(scope_for("variable", &["readonly".into()]), "constant");
        assert_eq!(
            scope_for("variable", &["readonly".into(), "defaultLibrary".into()]),
            "constant.builtin"
        );
        // A plain defaultLibrary variable → variable.builtin (a real palette entry).
        assert_eq!(
            scope_for("variable", &["defaultLibrary".into()]),
            "variable.builtin"
        );
    }
}
