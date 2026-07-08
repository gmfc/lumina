//! The `Command` vocabulary — every action the editor can take. Input, the palette, and
//! plugins all funnel through this into the single dispatcher in `app.rs` (plan §5,
//! "everything is a command"). Key → command id resolution lives in `keymap` + `commands`.

use std::path::PathBuf;

use editor_core::Motion;

/// Every action the editor can take. Input, menus, and the palette all funnel through this.
///
/// The full VS Code-style vocabulary is declared up front; variants are wired to input and
/// the dispatcher phase by phase (find/replace in Phase 6, palette/quick-open in Phase 7,
/// project search in Phase 8), so some are not yet constructed.
#[allow(dead_code)]
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
    MoveLineUp,
    MoveLineDown,
    ToggleComment,
    Indent,
    Outdent,
    // selection / motion (apply to ALL selections)
    Move(Motion),
    Extend(Motion),
    SelectAll,
    SelectWord,
    SelectLine,
    // multi-cursor
    AddCursorAbove,
    AddCursorBelow,
    AddCursorAtNextMatch,
    // clipboard
    Copy,
    Cut,
    Paste(String),
    // history
    Undo,
    Redo,
    // files / tabs
    OpenFile(PathBuf),
    Save,
    SaveAs,
    NewFile,
    CloseTab,
    NextTab,
    PrevTab,
    GotoTab(usize),
    // search
    FindOpen,
    FindNext,
    FindPrev,
    ReplaceOpen,
    ReplaceCurrent,
    ReplaceAll,
    ProjectSearch,
    // language server
    Hover,
    GotoDefinition,
    Completion,
    RenameSymbol,
    // ui
    ToggleSidebar,
    FocusSidebar,
    FocusEditor,
    Palette,
    QuickOpen,
    GotoLine,
    // registry command by id (plugin-contributed)
    Run(String),
    Quit,
}
