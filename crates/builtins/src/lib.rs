//! `editor_builtins` — the editor's own features, implemented **as plugins** over the
//! contribution API (CLAUDE.md invariant #3, plan §6A). Nothing here is privileged; each
//! feature reaches the editor only through `editor_plugin::Host`.
//!
//! The plugin-system migration is **complete**: every user-facing feature is a plugin here — the
//! explorer, multi-cursor, git-change nav, find/replace, palette + quick-open + goto-line, project
//! search, theme toggle, LSP request commands + nav/hover/rename responses, diagnostics,
//! completion, clipboard, terminal dock, and vim — each reaching the editor only through `Host`.
//! `editor-app` keeps only the substrate: editing primitives, file/tab IO, the PTY/vt100 + LSP
//! transports, and the pure renderer.
//!
//! The self-hosting test (`tests/self_hosting.rs`) guards each migrated plugin: with all built-ins
//! enabled its contributions are in the registry, and dropping the plugin removes exactly those and
//! nothing else.

use editor_plugin::Plugin;

pub mod clipboard;
pub mod code_action;
pub mod code_lens;
pub mod completion;
pub mod diagnostics;
pub mod document_highlight;
pub mod explorer;
pub mod find;
pub mod git_nav;
pub mod hover;
pub mod inlay_hints;
pub mod lsp;
pub mod lsp_nav;
pub mod multicursor;
pub mod palette;
pub mod project_search;
pub mod rename;
pub mod semantic_tokens;
pub mod signature_help;
pub mod snippet;
pub mod terminal;
pub mod theme;
pub mod vim;

/// The full set of built-in plugins, in registration order. `app` registers these; a user
/// config can filter the list to disable any of them (the litmus test for self-hosting).
pub fn all_builtins() -> Vec<Box<dyn Plugin>> {
    all_builtins_with(false)
}

/// Like [`all_builtins`], but with runtime options threaded in from user config (e.g. whether
/// the explorer draws Nerd Font glyphs).
pub fn all_builtins_with(icons: bool) -> Vec<Box<dyn Plugin>> {
    // Feature-by-feature migration onto the plugin system (docs/AUDIT.md roadmap): every
    // user-facing feature is now a plugin reaching the editor only through `Host` — the explorer,
    // multi-cursor, git-change navigation, find/replace, the command palette + quick-open +
    // goto-line, project search, the theme toggle, the LSP request commands, diagnostics,
    // completion, clipboard copy/cut/paste, LSP navigation + hover + rename, the terminal dock, and
    // the vim modal layer. `editor-app` keeps only the substrate: editing primitives, file/tab IO,
    // the PTY/vt100 transport, the LSP client, and the pure renderer.
    vec![
        Box::new(explorer::ExplorerPlugin::new(icons)),
        Box::new(multicursor::MultiCursorPlugin),
        Box::new(git_nav::GitNavPlugin),
        Box::new(find::FindReplacePlugin::default()),
        Box::new(palette::PalettePlugin::default()),
        Box::new(project_search::ProjectSearchPlugin::default()),
        Box::new(theme::ThemePlugin),
        Box::new(lsp::LspPlugin::default()),
        Box::new(lsp_nav::LspNavPlugin::default()),
        Box::new(hover::HoverPlugin),
        Box::new(signature_help::SignatureHelpPlugin::default()),
        Box::new(rename::RenamePlugin),
        Box::new(diagnostics::DiagnosticsPlugin::default()),
        Box::new(document_highlight::DocumentHighlightPlugin),
        Box::new(semantic_tokens::SemanticTokensPlugin),
        Box::new(inlay_hints::InlayHintsPlugin),
        Box::new(code_lens::CodeLensPlugin),
        Box::new(code_action::CodeActionPlugin::default()),
        Box::new(completion::CompletionPlugin::default()),
        Box::new(clipboard::ClipboardPlugin),
        Box::new(terminal::TerminalPlugin::default()),
        Box::new(vim::VimPlugin::default()),
    ]
}
