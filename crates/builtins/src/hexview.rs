//! Hex viewer, implemented **as a plugin** (invariant #3).
//!
//! The universal fallback for a file lumina refuses to open as text: a classic
//! `offset  hex bytes  |ascii|` dump. It understands no formats, so it works for all of them —
//! which is what makes every binary refusal actionable rather than a dead end.
//!
//! Claims no extensions: opening a `.png` should not silently become a hex dump. It is reached
//! explicitly through `view.openAsHex`, which the refusal notice advertises.
//!
//! Bounded by construction: it reads at most [`MAX_BYTES`] and says so, so dumping a 4 GB disk
//! image costs the same as dumping a 4 KB one.

use std::path::Path;

use editor_core::DocId;
use editor_plugin::{Contributions, Host, PanelLine, Plugin, Span, ViewerContent};

/// Bytes dumped at most. 256 KiB is 16 384 rows — far more than anyone scrolls, and small
/// enough that the render is instant.
const MAX_BYTES: usize = 256 * 1024;

/// Bytes per row, the universal hex-dump convention.
const COLS: usize = 16;

pub(crate) const VIEWER_ID: &str = "hexview.binary";

pub(crate) struct HexViewPlugin;

impl Plugin for HexViewPlugin {
    fn id(&self) -> &str {
        "hexview"
    }

    fn contributions(&self) -> Contributions {
        Contributions::builder()
            .command("view.openAsHex", "View: Open as Hex")
            .keybinding("ctrl+k ctrl+h", "view.openAsHex")
            .viewer(VIEWER_ID, "Hex View", &[])
            .build()
    }

    fn run_command(&mut self, command_id: &str, host: &mut dyn Host) -> bool {
        if command_id != "view.openAsHex" {
            return false;
        }
        // `active_path` (not the active *document*'s path) so this works from the refusal notice
        // tab, which is exactly where it is most useful and which has no text buffer.
        match host.active_path() {
            Some(path) => host.open_viewer(&path, VIEWER_ID),
            None => host.notify("Open a file first — there is nothing to view as hex".into()),
        }
        true
    }

    fn render_viewer(&mut self, viewer_id: &str, doc: DocId, path: &Path, host: &mut dyn Host) {
        if viewer_id != VIEWER_ID {
            return;
        }
        host.set_viewer_content(doc, dump_file(path));
    }
}

/// Read up to [`MAX_BYTES`] of `path` and render the dump, reporting any truncation.
fn dump_file(path: &Path) -> ViewerContent {
    let total = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    let bytes = match read_capped(path) {
        Ok(bytes) => bytes,
        Err(e) => return ViewerContent::status_only(format!("Could not read the file: {e}")),
    };
    let status = if (bytes.len() as u64) < total {
        format!(
            "{total} bytes — showing the first {} ({} rows)",
            bytes.len(),
            bytes.len().div_ceil(COLS)
        )
    } else {
        format!(
            "{} bytes ({} rows)",
            bytes.len(),
            bytes.len().div_ceil(COLS)
        )
    };
    ViewerContent {
        status: Some(status),
        lines: dump(&bytes),
    }
}

/// Read at most [`MAX_BYTES`] from `path`, so an enormous file costs a fixed amount of work.
fn read_capped(path: &Path) -> std::io::Result<Vec<u8>> {
    use std::io::Read;
    let mut buf = Vec::new();
    std::fs::File::open(path)?
        .take(MAX_BYTES as u64)
        .read_to_end(&mut buf)?;
    Ok(buf)
}

/// One styled row per 16 bytes: `00000010  48 65 6c 6c 6f …  |Hello…|`.
///
/// Split into three spans so the theme can dim the offset and the ASCII gutter while the bytes
/// themselves stay bright — the same trick the explorer uses for its icons.
fn dump(bytes: &[u8]) -> Vec<PanelLine> {
    let mut lines = Vec::with_capacity(bytes.len().div_ceil(COLS));
    for (row, chunk) in bytes.chunks(COLS).enumerate() {
        let offset = row * COLS;
        // Fixed-width hex column (with an extra gap mid-row), padded so short final chunks keep
        // the ASCII gutter aligned.
        let mut hex = String::with_capacity(COLS * 3 + 1);
        for i in 0..COLS {
            if i == COLS / 2 {
                hex.push(' ');
            }
            match chunk.get(i) {
                Some(b) => hex.push_str(&format!("{b:02x} ")),
                None => hex.push_str("   "),
            }
        }
        let ascii: String = chunk
            .iter()
            .map(|&b| {
                if (0x20..0x7f).contains(&b) {
                    b as char
                } else {
                    '.'
                }
            })
            .collect();
        lines.push(PanelLine::new(vec![
            Span::new(format!("{offset:08x}  "), "dim"),
            Span::new(hex, "text"),
            Span::new(format!(" |{ascii}|"), "dim"),
        ]));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row_text(line: &PanelLine) -> String {
        line.spans.iter().map(|s| s.text.as_str()).collect()
    }

    #[test]
    fn dumps_offset_bytes_and_ascii() {
        let lines = dump(b"Hello");
        assert_eq!(lines.len(), 1);
        let row = row_text(&lines[0]);
        assert!(row.starts_with("00000000  "), "8-digit offset: {row}");
        assert!(row.contains("48 65 6c 6c 6f"), "hex bytes: {row}");
        assert!(row.ends_with("|Hello|"), "ascii gutter: {row}");
    }

    #[test]
    fn non_printable_bytes_become_dots() {
        let row = row_text(&dump(&[0x00, 0x1f, 0x7f, 0xff])[0]);
        assert!(row.ends_with("|....|"), "{row}");
        assert!(row.contains("00 1f 7f ff"), "{row}");
    }

    #[test]
    fn rows_are_sixteen_bytes_and_offsets_advance() {
        let lines = dump(&[b'x'; 33]);
        assert_eq!(lines.len(), 3, "33 bytes is 3 rows of 16");
        assert!(row_text(&lines[1]).starts_with("00000010"));
        assert!(row_text(&lines[2]).starts_with("00000020"));
    }

    #[test]
    fn a_short_final_row_keeps_the_ascii_gutter_aligned() {
        // The padding matters: without it the last row's `|…|` would slide left and the column
        // would zig-zag.
        let lines = dump(&[b'a'; 17]);
        let (full, short) = (row_text(&lines[0]), row_text(&lines[1]));
        let col = |s: &str| s.find('|').unwrap();
        assert_eq!(col(&full), col(&short), "ascii gutter column drifted");
    }

    #[test]
    fn empty_input_dumps_nothing() {
        assert!(dump(&[]).is_empty());
    }

    #[test]
    fn reading_is_capped_and_reported() {
        let path = std::env::temp_dir().join(format!("lumina_hex_{}.bin", std::process::id()));
        std::fs::write(&path, vec![0u8; MAX_BYTES + 4096]).unwrap();
        let content = dump_file(&path);
        std::fs::remove_file(&path).ok();
        assert_eq!(content.lines.len(), MAX_BYTES / COLS, "capped at MAX_BYTES");
        assert!(
            content
                .status
                .as_deref()
                .is_some_and(|s| s.contains("first")),
            "truncation is stated: {:?}",
            content.status
        );
    }

    #[test]
    fn an_unreadable_file_reports_instead_of_panicking() {
        let content = dump_file(std::path::Path::new("/definitely/not/here.bin"));
        assert!(content.lines.is_empty());
        assert!(content.status.is_some());
    }
}
