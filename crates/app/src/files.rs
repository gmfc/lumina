//! File IO: loading into a `Document` (detecting encoding/line-endings) and **atomic**
//! saves (temp + rename) so crashes and external readers never see a partial write
//! (CLAUDE.md invariant #9).

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use editor_core::document::DiskFingerprint;
use editor_core::{Document, Encoding, LineEnding};

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

/// Decode raw file bytes into text, detecting a UTF-8 BOM or UTF-16 LE/BE by BOM.
/// Falls back to lossy UTF-8 so we never fail to open a file (plan §3, encoding).
pub fn decode(bytes: &[u8]) -> (String, Encoding) {
    if let Some(rest) = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        (
            String::from_utf8_lossy(rest).into_owned(),
            Encoding::Utf8Bom,
        )
    } else if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        (decode_utf16(rest, false), Encoding::Utf16Le)
    } else if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        (decode_utf16(rest, true), Encoding::Utf16Be)
    } else {
        (String::from_utf8_lossy(bytes).into_owned(), Encoding::Utf8)
    }
}

fn decode_utf16(bytes: &[u8], be: bool) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| {
            if be {
                u16::from_be_bytes([c[0], c[1]])
            } else {
                u16::from_le_bytes([c[0], c[1]])
            }
        })
        .collect();
    String::from_utf16_lossy(&units)
}

/// Load a file into a `Document`, recording its path, language, encoding, and fingerprint.
pub fn load(path: &Path) -> Result<Document> {
    let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let fp = fingerprint(&bytes);
    let (text, encoding) = decode(&bytes);
    let mut doc = Document::from_str(&text);
    doc.path = Some(path.to_path_buf());
    doc.language = language_for(path);
    doc.encoding = encoding;
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

/// Encode a document's text back to its detected on-disk encoding, re-emitting any BOM.
pub fn encode(doc: &Document) -> Vec<u8> {
    let text = serialize(doc);
    match doc.encoding {
        Encoding::Utf8 => text.into_bytes(),
        Encoding::Utf8Bom => {
            let mut v = vec![0xEF, 0xBB, 0xBF];
            v.extend_from_slice(text.as_bytes());
            v
        }
        Encoding::Utf16Le => encode_utf16(&text, false),
        Encoding::Utf16Be => encode_utf16(&text, true),
    }
}

fn encode_utf16(text: &str, be: bool) -> Vec<u8> {
    let mut v = if be {
        vec![0xFE, 0xFF]
    } else {
        vec![0xFF, 0xFE]
    };
    for u in text.encode_utf16() {
        let b = if be { u.to_be_bytes() } else { u.to_le_bytes() };
        v.extend_from_slice(&b);
    }
    v
}

/// Atomically write `doc` to `path`: write a temp file in the same directory, then rename.
/// Returns the fingerprint of the bytes written (for save-echo suppression).
pub fn save(doc: &Document, path: &Path) -> Result<DiskFingerprint> {
    let bytes = encode(doc);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_encodings_by_bom() {
        assert_eq!(decode(b"hello"), ("hello".into(), Encoding::Utf8));

        let mut bom = vec![0xEF, 0xBB, 0xBF];
        bom.extend_from_slice("hi".as_bytes());
        assert_eq!(decode(&bom), ("hi".into(), Encoding::Utf8Bom));

        let le = encode_utf16("héllo", false);
        assert_eq!(decode(&le), ("héllo".into(), Encoding::Utf16Le));
        let be = encode_utf16("héllo", true);
        assert_eq!(decode(&be), ("héllo".into(), Encoding::Utf16Be));
    }

    #[test]
    fn encode_round_trips_each_encoding() {
        for enc in [
            Encoding::Utf8,
            Encoding::Utf8Bom,
            Encoding::Utf16Le,
            Encoding::Utf16Be,
        ] {
            let mut doc = Document::from_str("line1\nldiné2");
            doc.encoding = enc;
            let bytes = encode(&doc);
            let (text, detected) = decode(&bytes);
            assert_eq!(detected, enc, "encoding preserved for {enc:?}");
            assert_eq!(text, "line1\nldiné2", "text preserved for {enc:?}");
        }
    }

    #[test]
    fn crlf_reencoded_on_save() {
        let doc = Document::from_str("a\r\nb"); // detected CRLF, stored LF internally
        let bytes = encode(&doc);
        assert_eq!(bytes, b"a\r\nb");
    }
}
