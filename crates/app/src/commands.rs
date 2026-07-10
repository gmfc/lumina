//! The built-in command table: `id → title` (for the palette) and `id → Command` (for the
//! dispatcher). This is the seed unification from plan §6A — every built-in action has a
//! stable id, so keys can be remapped to it and the palette can list it, exactly like a
//! plugin-contributed command.

use editor_core::Motion;

use crate::input::Command;

mod tables;

pub use tables::{default_bindings, palette_entries};

/// Resolve a command id to a concrete [`Command`], if it's a built-in.
pub fn command_for_id(id: &str) -> Option<Command> {
    use Motion::*;
    let cmd = match id {
        // motion
        "cursor.left" => Command::Move(Left),
        "cursor.right" => Command::Move(Right),
        "cursor.up" => Command::Move(Up),
        "cursor.down" => Command::Move(Down),
        "cursor.wordLeft" => Command::Move(WordLeft),
        "cursor.wordRight" => Command::Move(WordRight),
        "cursor.lineStart" => Command::Move(LineStart),
        "cursor.lineEnd" => Command::Move(LineEnd),
        "cursor.docStart" => Command::Move(DocStart),
        "cursor.docEnd" => Command::Move(DocEnd),
        "cursor.pageUp" => Command::Move(PageUp),
        "cursor.pageDown" => Command::Move(PageDown),
        // selection extend
        "select.left" => Command::Extend(Left),
        "select.right" => Command::Extend(Right),
        "select.up" => Command::Extend(Up),
        "select.down" => Command::Extend(Down),
        "select.lineStart" => Command::Extend(LineStart),
        "select.lineEnd" => Command::Extend(LineEnd),
        "select.wordLeft" => Command::Extend(WordLeft),
        "select.wordRight" => Command::Extend(WordRight),
        "cursor.jumpToBracket" => Command::Move(MatchingBracket),
        "select.toBracket" => Command::Extend(MatchingBracket),
        "edit.selectAll" => Command::SelectAll,
        "edit.selectWord" => Command::SelectWord,
        "edit.selectLine" => Command::SelectLine,
        // editing
        "edit.newline" => Command::InsertNewline,
        "edit.deleteBackward" => Command::DeleteBackward,
        "edit.deleteForward" => Command::DeleteForward,
        "edit.deleteWordBackward" => Command::DeleteWordBackward,
        "edit.duplicateLine" => Command::DuplicateLine,
        "edit.copyLineUp" => Command::CopyLineUp,
        "edit.deleteLines" => Command::DeleteLine,
        "edit.insertLineBelow" => Command::InsertLineBelow,
        "edit.insertLineAbove" => Command::InsertLineAbove,
        "edit.moveLineUp" => Command::MoveLineUp,
        "edit.moveLineDown" => Command::MoveLineDown,
        "edit.toggleComment" => Command::ToggleComment,
        "edit.trimTrailingWhitespace" => Command::TrimTrailingWhitespace,
        "edit.indent" => Command::Indent,
        "edit.outdent" => Command::Outdent,
        "edit.undo" => Command::Undo,
        "edit.redo" => Command::Redo,
        "edit.copy" => Command::Copy,
        "edit.cut" => Command::Cut,
        "edit.paste" => Command::Paste(String::new()),
        // multi-cursor is the `multicursor` builtin plugin — all cursor.* ids dispatch through
        // the registry, not here.
        // files / tabs
        "file.save" => Command::Save,
        "file.saveAs" => Command::SaveAs,
        "file.saveAll" => Command::SaveAll,
        "file.new" => Command::NewFile,
        "tab.close" => Command::CloseTab,
        "tab.closeAll" => Command::CloseAllTabs,
        "tab.reopenClosed" => Command::ReopenClosedTab,
        "tab.next" => Command::NextTab,
        "tab.prev" => Command::PrevTab,
        "tab.goto1" => Command::GotoTab(0),
        "tab.goto2" => Command::GotoTab(1),
        "tab.goto3" => Command::GotoTab(2),
        "tab.goto4" => Command::GotoTab(3),
        "tab.goto5" => Command::GotoTab(4),
        "tab.goto6" => Command::GotoTab(5),
        "tab.goto7" => Command::GotoTab(6),
        "tab.goto8" => Command::GotoTab(7),
        "tab.goto9" => Command::GotoTab(8),
        // search.* (find/replace + project search) are builtin plugins now.
        // language server
        "lsp.hover" => Command::Hover,
        "lsp.gotoDefinition" => Command::GotoDefinition,
        "lsp.gotoImplementation" => Command::GotoImplementation,
        "lsp.gotoTypeDefinition" => Command::GotoTypeDefinition,
        "lsp.completion" => Command::Completion,
        "lsp.rename" => Command::RenameSymbol,
        "lsp.nextDiagnostic" => Command::NextDiagnostic,
        "lsp.prevDiagnostic" => Command::PrevDiagnostic,
        "lsp.references" => Command::FindReferences,
        "lsp.documentSymbols" => Command::DocumentSymbols,
        // git.nextHunk / git.prevHunk are contributed by the `git-nav` builtin plugin
        // ui
        "view.toggleSidebar" => Command::ToggleSidebar,
        "view.focusSidebar" => Command::FocusSidebar,
        "view.focusEditor" => Command::FocusEditor,
        // view.commandPalette / view.quickOpen / view.gotoLine are the `palette` plugin now.
        // terminal panel
        "terminal.toggle" => Command::ToggleTerminal,
        "terminal.new" => Command::NewTerminal,
        "terminal.close" => Command::CloseTerminal,
        "terminal.minimize" => Command::MinimizeTerminal,
        "terminal.next" => Command::NextTerminal,
        "terminal.prev" => Command::PrevTerminal,
        "app.quit" => Command::Quit,
        _ => return None,
    };
    Some(cmd)
}
