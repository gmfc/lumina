//! File IO: loading into a `Document` (detecting encoding/line-endings) and **atomic**
//! saves (temp + rename) so crashes and external readers never see a partial write
//! (CLAUDE.md invariant #9).

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use editor_core::document::DiskFingerprint;
use editor_core::{Document, LineEnding};

/// Cheap content hash used for change detection + save-echo suppression.
pub fn fingerprint(bytes: &[u8]) -> DiskFingerprint {
    DiskFingerprint {
        hash: seahash_hash(bytes),
        len: bytes.len(),
    }
}

// A tiny dependency-free 64-bit hash (FNV-1a) — good enough to detect real changes.
fn seahash_hash(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// Map a file extension to a language id (drives syntax highlighting later).
pub fn language_for(path: &Path) -> Option<String> {
    let ext = path.extension()?.to_str()?;
    let lang = match ext {
        "rs" => "rust",
        "py" => "python",
        "js" | "mjs" | "cjs" => "javascript",
        "ts" => "typescript",
        "json" => "json",
        "toml" => "toml",
        "md" | "markdown" => "markdown",
        "c" | "h" => "c",
        "go" => "go",
        _ => return None,
    };
    Some(lang.to_string())
}

/// Load a file into a `Document`, recording its path, language, and disk fingerprint.
pub fn load(path: &Path) -> Result<Document> {
    let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let fp = fingerprint(&bytes);
    // UTF-8 with lossy fallback so we never fail to open a file.
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let mut doc = Document::from_str(&text);
    doc.path = Some(path.to_path_buf());
    doc.language = language_for(path);
    doc.disk = fp;
    Ok(doc)
}

/// Serialize a document's text back to the file's original line-ending style.
pub fn serialize(doc: &Document) -> String {
    let text = doc.to_string();
    match doc.line_ending {
        LineEnding::Lf => text,
        LineEnding::Crlf => text.replace('\n', "\r\n"),
    }
}

/// Atomically write `doc` to `path`: write a temp file in the same directory, then rename.
/// Returns the fingerprint of the bytes written (for save-echo suppression).
pub fn save(doc: &Document, path: &Path) -> Result<DiskFingerprint> {
    let content = serialize(doc);
    let bytes = content.into_bytes();
    let fp = fingerprint(&bytes);

    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    let tmp = temp_path(path);
    {
        let mut f =
            fs::File::create(&tmp).with_context(|| format!("creating temp {}", tmp.display()))?;
        f.write_all(&bytes)?;
        f.sync_all()?;
    }
    // Rename is atomic on the same filesystem.
    fs::rename(&tmp, path).with_context(|| format!("renaming into {}", path.display()))?;
    let _ = dir; // dir kept for clarity; rename target already includes it.
    Ok(fp)
}

fn temp_path(path: &Path) -> PathBuf {
    let mut name = path
        .file_name()
        .map(|n| n.to_os_string())
        .unwrap_or_default();
    name.push(".lumina.tmp");
    path.with_file_name(name)
}
