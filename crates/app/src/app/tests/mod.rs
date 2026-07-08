use super::*;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent};
use editor_core::Motion;
use ratatui::layout::Rect;
use std::sync::atomic::{AtomicU32, Ordering};

mod editing;
mod explorer;
mod files;
mod find_search;
mod lsp;
mod mouse;
mod palette_theme;
mod plugins;
mod render;
mod render_overlays;
mod sync;
mod terminal;

static COUNTER: AtomicU32 = AtomicU32::new(0);

fn temp_file(contents: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let mut p = std::env::temp_dir();
    p.push(format!("lumina_test_{}_{}.txt", std::process::id(), n));
    std::fs::write(&p, contents).unwrap();
    p
}

fn app_with(path: &std::path::Path) -> App {
    App::new(Some(path.to_string_lossy().into_owned())).unwrap()
}

fn temp_dir_with_files() -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let mut dir = std::env::temp_dir();
    dir.push(format!("lumina_dir_{}_{}", std::process::id(), n));
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    std::fs::write(dir.join("a.txt"), "alpha").unwrap();
    std::fs::write(dir.join("sub").join("b.txt"), "beta").unwrap();
    dir
}

fn mouse(kind: MouseEventKind, col: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column: col,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

fn ci(label: &str, kind: u8) -> editor_lsp::CompletionItem {
    editor_lsp::CompletionItem {
        label: label.to_string(),
        detail: None,
        insert_text: label.to_string(),
        kind: Some(kind),
    }
}

fn diag(line: u32, sc: u32, el: u32, ec: u32, msg: &str) -> editor_lsp::Diagnostic {
    editor_lsp::Diagnostic {
        line,
        start_char16: sc,
        end_line: el,
        end_char16: ec,
        severity: editor_lsp::Severity::Error,
        message: msg.to_string(),
    }
}

/// Create a project dir containing a `.lumina/plugins/<id>` plugin and a file to open.
fn temp_project_with_plugin(
    id: &str,
    manifest: &str,
    script: &str,
    file_contents: &str,
) -> (PathBuf, PathBuf) {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let mut dir = std::env::temp_dir();
    dir.push(format!("lumina_plugin_{}_{}", std::process::id(), n));
    let pdir = dir.join(".lumina").join("plugins").join(id);
    std::fs::create_dir_all(&pdir).unwrap();
    std::fs::write(pdir.join("plugin.toml"), manifest).unwrap();
    std::fs::write(pdir.join("main.rhai"), script).unwrap();
    let file = dir.join("doc.txt");
    std::fs::write(&file, file_contents).unwrap();
    (dir, file)
}

fn render_to_string(app: &mut App, w: u16, h: u16) -> String {
    use ratatui::{backend::TestBackend, Terminal};
    let backend = TestBackend::new(w, h);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal.draw(|f| crate::ui::draw(f, app)).unwrap();
    terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|c| c.symbol().to_string())
        .collect()
}

/// Drain PTY output and redraw until `needle` renders, or a short timeout elapses.
#[cfg(target_os = "linux")]
fn pump_until(app: &mut App, needle: &str) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        app.drain_workers();
        if render_to_string(app, 120, 40).contains(needle) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    false
}
