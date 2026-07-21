//! The `Command` vocabulary — the editing/selection/history primitives plus the file/tab/UI
//! actions the app dispatches directly in `app.rs` `dispatch()`. Every user-facing *feature* is a
//! builtin plugin routed through the registry, not a variant here; command ids resolve
//! registry-first in `exec_id` (`registry.dispatch_command`), then fall back to `command_for_id`
//! for these primitives. Key → id resolution lives in `keymap` + `commands`.

use editor_core::Motion;

/// The editing/selection/history primitives + file/tab/UI actions dispatched directly by
/// `dispatch()`. Feature commands (find, palette, project-search, LSP, diagnostics, git-nav,
/// terminal, vim, clipboard, multicursor) are plugin-contributed and reach the editor through the
/// registry + `Host`, not through variants here.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    // editing
    InsertChar(char),
    InsertNewline,
    InsertText(String),
    DeleteBackward,
    DeleteForward,
    DeleteWordBackward,
    DuplicateLine,
    CopyLineUp,
    DeleteLine,
    InsertLineBelow,
    InsertLineAbove,
    MoveLineUp,
    MoveLineDown,
    ToggleComment,
    TrimTrailingWhitespace,
    Indent,
    Outdent,
    // selection / motion (apply to ALL selections)
    Move(Motion),
    Extend(Motion),
    SelectAll,
    SelectWord,
    SelectLine,
    // multi-cursor is entirely the `multicursor` builtin plugin now (add-next-match / select-all
    // / add-above-below / add-cursors-to-line-ends). clipboard copy/cut/paste is the `clipboard`
    // builtin plugin.
    // history
    Undo,
    Redo,
    // files / tabs
    Save,
    SaveAs,
    SaveAll,
    NewFile,
    CloseTab,
    CloseAllTabs,
    ReopenClosedTab,
    NextTab,
    PrevTab,
    GotoTab(usize),
    // search — find/replace + project search are builtin plugins now.
    // language server: request commands are the `lsp` plugin; diagnostic navigation is the
    // `diagnostics` plugin. git change navigation (NextHunk/PrevHunk) is the `git-nav` plugin.
    // ui
    ToggleSidebar,
    ToggleWrap,
    FocusSidebar,
    FocusEditor,
    // Command palette, quick-open, and goto-line are the `palette` plugin now.
    // terminal-dock commands are the `terminal` plugin now (PTY/render stay app-side).
    Quit,
}
