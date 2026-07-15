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
        hash: fnv1a_hash(bytes),
        len: bytes.len(),
    }
}

// A tiny dependency-free 64-bit hash (FNV-1a) — good enough to detect real changes.
fn fnv1a_hash(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        hash ^= b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// Map a file extension to a language id. This one id drives both syntax highlighting
/// ([`editor_syntax::lang`]) and LSP server selection ([`crate::lsp::registry`]), so `.tsx`/`.jsx`
/// map to `typescript`/`javascript` (highlight-compatible ids that tsserver still serves) rather
/// than the LSP-spec `typescriptreact`/`javascriptreact`, which have no grammar wired.
pub fn language_for(path: &Path) -> Option<String> {
    let ext = path.extension()?.to_str()?;
    let lang = match ext {
        "rs" => "rust",
        "py" | "pyi" => "python",
        "js" | "mjs" | "cjs" | "jsx" => "javascript",
        "ts" | "mts" | "cts" | "tsx" => "typescript",
        "json" | "jsonc" => "json",
        "toml" => "toml",
        "md" | "markdown" => "markdown",
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hpp" | "hh" | "hxx" => "cpp",
        "go" => "go",
        "java" => "java",
        "kt" | "kts" => "kotlin",
        "rb" => "ruby",
        "php" => "php",
        "cs" => "csharp",
        "lua" => "lua",
        "sh" | "bash" | "zsh" => "bash",
        "yaml" | "yml" => "yaml",
        "html" | "htm" => "html",
        "css" => "css",
        "scss" | "less" => "scss",
        "sql" => "sql",
        "swift" => "swift",
        "zig" => "zig",
        _ => return None,
    };
    Some(lang.to_string())
}

/// Walk up from `start` to the nearest ancestor holding a project marker (a VCS dir or a language
/// manifest), returning it as the LSP root. Falls back to `start` when none is found, so a language
/// server always gets a sane `rootUri` even for a loose file — this is what lets rust-analyzer work
/// when a file is opened deep inside a workspace rather than from its root.
pub fn project_root(start: &Path) -> PathBuf {
    const MARKERS: &[&str] = &[
        ".git",
        "Cargo.toml",
        "package.json",
        "go.mod",
        "pyproject.toml",
        "tsconfig.json",
        "pom.xml",
        "build.gradle",
        "composer.json",
    ];
    let mut dir = start;
    loop {
        if MARKERS.iter().any(|m| dir.join(m).exists()) {
            return dir.to_path_buf();
        }
        match dir.parent() {
            Some(parent) => dir = parent,
            None => return start.to_path_buf(),
        }
    }
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

/// Make a path absolute without touching the filesystem. Every open document's path is stored
/// absolute so the `file://` URIs sent to language servers are well-formed — a *relative* path
/// yields `file://rel/dir/file.rs`, where the first segment is parsed as the URL host, and servers
/// (rust-analyzer) reject it as "url is not a file". Falls back to the input if the cwd is
/// unavailable.
pub fn absolute_path(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

/// Load a file into a `Document`, recording its (absolute) path, language, encoding, and fingerprint.
pub fn load(path: &Path) -> Result<Document> {
    let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let fp = fingerprint(&bytes);
    let (text, encoding) = decode(&bytes);
    let mut doc = Document::from_str(&text);
    doc.path = Some(absolute_path(path));
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
    // Preserve the original file's permissions across the temp+rename: a fresh temp file gets
    // default umask perms, which would otherwise silently strip e.g. a script's executable bit.
    preserve_mode(path, &tmp);
    // Rename is atomic on the same filesystem.
    fs::rename(&tmp, path).with_context(|| format!("renaming into {}", path.display()))?;
    let _ = dir; // dir kept for clarity; rename target already includes it.
    Ok(fp)
}

/// Copy `src`'s permission bits onto `dst` when `src` already exists (a resave). Best-effort:
/// a metadata/permission error must not fail the save. No-op on non-Unix targets, where the
/// executable bit isn't file-mode-based.
#[cfg(unix)]
fn preserve_mode(src: &Path, dst: &Path) {
    if let Ok(meta) = fs::metadata(src) {
        let _ = fs::set_permissions(dst, meta.permissions());
    }
}

#[cfg(not(unix))]
fn preserve_mode(_src: &Path, _dst: &Path) {}

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

    #[test]
    fn language_for_maps_every_known_extension() {
        // Exhaustive: one representative per match arm, so the whole mapping is exercised and the
        // `.tsx`/`.jsx` → highlight-compatible-id decision is pinned.
        let cases = [
            ("a.rs", "rust"),
            ("a.py", "python"),
            ("a.pyi", "python"),
            ("a.js", "javascript"),
            ("a.mjs", "javascript"),
            ("a.cjs", "javascript"),
            ("a.jsx", "javascript"),
            ("a.ts", "typescript"),
            ("a.mts", "typescript"),
            ("a.cts", "typescript"),
            ("a.tsx", "typescript"),
            ("a.json", "json"),
            ("a.jsonc", "json"),
            ("a.toml", "toml"),
            ("a.md", "markdown"),
            ("a.markdown", "markdown"),
            ("a.c", "c"),
            ("a.h", "c"),
            ("a.cc", "cpp"),
            ("a.cpp", "cpp"),
            ("a.cxx", "cpp"),
            ("a.hpp", "cpp"),
            ("a.hh", "cpp"),
            ("a.hxx", "cpp"),
            ("a.go", "go"),
            ("a.java", "java"),
            ("a.kt", "kotlin"),
            ("a.kts", "kotlin"),
            ("a.rb", "ruby"),
            ("a.php", "php"),
            ("a.cs", "csharp"),
            ("a.lua", "lua"),
            ("a.sh", "bash"),
            ("a.bash", "bash"),
            ("a.zsh", "bash"),
            ("a.yaml", "yaml"),
            ("a.yml", "yaml"),
            ("a.html", "html"),
            ("a.htm", "html"),
            ("a.css", "css"),
            ("a.scss", "scss"),
            ("a.less", "scss"),
            ("a.sql", "sql"),
            ("a.swift", "swift"),
            ("a.zig", "zig"),
        ];
        for (path, lang) in cases {
            assert_eq!(
                language_for(Path::new(path)).as_deref(),
                Some(lang),
                "{path} should map to {lang}"
            );
        }
        // An unknown extension (and a no-extension path) yields no language.
        assert_eq!(language_for(Path::new("a.unknownext")), None);
        assert_eq!(language_for(Path::new("README")), None);
    }

    #[test]
    fn absolute_path_makes_file_uris_well_formed() {
        // A relative path becomes absolute, so `uri_for` yields `file:///…` rather than the
        // malformed `file://rel/…` that servers reject as "url is not a file".
        let abs = absolute_path(Path::new("crates/app/src/app.rs"));
        assert!(abs.is_absolute(), "relative paths are absolutized");
        // The exact `file://` form is platform-specific (Windows absolute paths are `C:\…`); the
        // `file:///…` shape is the Unix one that servers on the user's platform expect.
        #[cfg(unix)]
        {
            let uri = crate::lsp::uri_for(&abs);
            assert!(uri.starts_with("file:///"), "well-formed file URI: {uri}");
        }
        // An already-absolute path stays absolute.
        assert!(absolute_path(&std::env::temp_dir()).is_absolute());
    }

    #[test]
    fn loaded_document_has_an_absolute_path() {
        let p = std::env::temp_dir().join(format!("lumina_abs_{}.rs", std::process::id()));
        std::fs::write(&p, "fn main() {}\n").unwrap();
        let doc = load(&p).unwrap();
        assert!(
            doc.path.as_ref().unwrap().is_absolute(),
            "a loaded document's path is stored absolute"
        );
        std::fs::remove_file(&p).ok();
    }

    #[test]
    fn project_root_walks_up_to_the_nearest_marker() {
        let base = std::env::temp_dir().join(format!("lumina_root_{}", std::process::id()));
        let nested = base.join("crates").join("app").join("src");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(base.join("Cargo.toml"), b"[workspace]\n").unwrap();

        // From deep inside the tree, the root resolves to the dir holding the manifest.
        assert_eq!(project_root(&nested), base);
        // From the marker dir itself, it returns that dir.
        assert_eq!(project_root(&base), base);
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn project_root_falls_back_to_start_when_no_marker() {
        // A directory with no marker anywhere up the (temp) tree falls back to itself.
        let dir = std::env::temp_dir().join(format!("lumina_noroot_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        // No marker in `dir`; `project_root` may find one further up the real filesystem, so only
        // assert it returns *some* existing ancestor (never panics, always a real dir).
        let root = project_root(&dir);
        assert!(root.exists(), "project_root returns an existing directory");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn save_preserves_unix_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let path = std::env::temp_dir().join(format!(
            "lumina_perm_test_{}_{}.sh",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        fs::write(&path, "#!/bin/sh\necho hi\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();

        let mut doc = Document::from_str("#!/bin/sh\necho bye\n");
        doc.path = Some(path.clone());
        save(&doc, &path).unwrap();

        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        fs::remove_file(&path).ok();
        assert_eq!(mode, 0o755, "executable bit was dropped on save");
    }
}
