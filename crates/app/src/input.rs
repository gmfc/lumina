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
    // multi-cursor (add-next-match / select-all / add-above-below now live in the
    // `multicursor` builtin plugin; only the line-ends variant remains app-side)
    CursorsToLineEnds,
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
    SaveAll,
    NewFile,
    CloseTab,
    CloseAllTabs,
    ReopenClosedTab,
    NextTab,
    PrevTab,
    GotoTab(usize),
    // search — find/replace is now the `find` builtin plugin (search.* commands); only
    // project-wide search remains an app command.
    ProjectSearch,
    // language server
    Hover,
    GotoDefinition,
    GotoImplementation,
    GotoTypeDefinition,
    Completion,
    RenameSymbol,
    NextDiagnostic,
    PrevDiagnostic,
    FindReferences,
    DocumentSymbols,
    // git change navigation (NextHunk/PrevHunk) is the `git-nav` builtin plugin
    // ui
    ToggleSidebar,
    FocusSidebar,
    FocusEditor,
    // Command palette, quick-open, and goto-line are the `palette` plugin now.
    // terminal panel
    ToggleTerminal,
    NewTerminal,
    CloseTerminal,
    MinimizeTerminal,
    NextTerminal,
    PrevTerminal,
    // registry command by id (plugin-contributed)
    Run(String),
    Quit,
}
