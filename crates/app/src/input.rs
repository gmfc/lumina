//! Input → `Command`. Keys and mouse events resolve to a [`Command`]; a single dispatcher
//! (in `app.rs`) is the only place state mutates (plan §5, "everything is a command").
//!
//! Phase 7 generalizes this into a chord-trie keymap loaded from config; until then a
//! direct match provides the default VS Code-ish bindings.

use std::path::PathBuf;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
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
    Indent,
    Outdent,
    // selection / motion (apply to ALL selections)
    Move(Motion),
    Extend(Motion),
    SelectAll,
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
    CloseTab,
    NextTab,
    PrevTab,
    GotoTab(usize),
    // search
    FindOpen,
    FindNext,
    FindPrev,
    ReplaceOpen,
    ProjectSearch,
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

/// The focused region, used to route some keys (e.g. arrows in the tree vs the editor).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Editor,
    Sidebar,
}

/// Map a key press to a command given the current focus. Returns `None` if unbound.
pub fn key_to_command(key: KeyEvent, focus: Focus) -> Option<Command> {
    let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    let alt = key.modifiers.contains(KeyModifiers::ALT);

    // Global chords first.
    if ctrl {
        match key.code {
            KeyCode::Char('q') => return Some(Command::Quit),
            KeyCode::Char('s') => return Some(Command::Save),
            KeyCode::Char('w') => return Some(Command::CloseTab),
            KeyCode::Char('b') => return Some(Command::ToggleSidebar),
            KeyCode::Char('z') => return Some(Command::Undo),
            KeyCode::Char('y') => return Some(Command::Redo),
            KeyCode::Char('a') => return Some(Command::SelectAll),
            KeyCode::Char('f') if shift => return Some(Command::ProjectSearch),
            KeyCode::Char('f') => return Some(Command::FindOpen),
            KeyCode::Char('h') => return Some(Command::ReplaceOpen),
            KeyCode::Char('p') if shift => return Some(Command::Palette),
            KeyCode::Char('p') => return Some(Command::QuickOpen),
            KeyCode::Char('g') => return Some(Command::GotoLine),
            KeyCode::Char('d') => return Some(Command::AddCursorAtNextMatch),
            KeyCode::Char('c') => return Some(Command::Copy),
            KeyCode::Char('x') => return Some(Command::Cut),
            KeyCode::Char('v') => return Some(Command::Paste(String::new())),
            KeyCode::Tab => {
                return Some(if shift {
                    Command::PrevTab
                } else {
                    Command::NextTab
                })
            }
            KeyCode::Char(c @ '1'..='9') => {
                return Some(Command::GotoTab(c as usize - '1' as usize))
            }
            KeyCode::Right => return Some(Command::Move(Motion::WordRight)),
            KeyCode::Left => return Some(Command::Move(Motion::WordLeft)),
            KeyCode::Home => return Some(Command::Move(Motion::DocStart)),
            KeyCode::End => return Some(Command::Move(Motion::DocEnd)),
            _ => {}
        }
    }

    // Alt+Up/Down move lines (Phase 9); for now map to add-cursor.
    if alt {
        match key.code {
            KeyCode::Up => return Some(Command::AddCursorAbove),
            KeyCode::Down => return Some(Command::AddCursorBelow),
            _ => {}
        }
    }

    // Motion / extend with Shift.
    let motion = match key.code {
        KeyCode::Left => Some(Motion::Left),
        KeyCode::Right => Some(Motion::Right),
        KeyCode::Up => Some(Motion::Up),
        KeyCode::Down => Some(Motion::Down),
        KeyCode::Home => Some(Motion::LineStart),
        KeyCode::End => Some(Motion::LineEnd),
        KeyCode::PageUp => Some(Motion::PageUp),
        KeyCode::PageDown => Some(Motion::PageDown),
        _ => None,
    };
    if let Some(m) = motion {
        return Some(if shift {
            Command::Extend(m)
        } else {
            Command::Move(m)
        });
    }

    // Text entry only when the editor is focused.
    if focus == Focus::Editor {
        match key.code {
            KeyCode::Char(c) if !ctrl && !alt => return Some(Command::InsertChar(c)),
            KeyCode::Enter => return Some(Command::InsertNewline),
            KeyCode::Backspace => return Some(Command::DeleteBackward),
            KeyCode::Delete => return Some(Command::DeleteForward),
            KeyCode::Tab => return Some(Command::Indent),
            KeyCode::BackTab => return Some(Command::Outdent),
            _ => {}
        }
    }

    None
}
