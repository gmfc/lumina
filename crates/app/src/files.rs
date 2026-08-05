//! File IO: classifying a file *before* reading it, loading into a `Document` (detecting
//! encoding/line-endings) and **atomic** saves (temp + rename) so crashes and external readers
//! never see a partial write (CLAUDE.md invariant #9).
//!
//! [`open`] is the single chokepoint every "path → tab" route goes through. It probes the head
//! of the file first ([`probe`]) so a 2 GB video or a 40 MB PDF is classified in O(header) and
//! never slurped into a rope: turning arbitrary bytes into a text buffer costs four full passes
//! (read → lossy-decode → CRLF-normalize → rope build) plus an O(n) hash, all on the UI thread,
//! and yields a buffer of replacement characters that would corrupt the file if saved.

use std::fs;
use std::io::{Read, Write};
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
    use std::path::Component;
    let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    // `std::path::absolute` joins with the cwd but keeps `.`/`..` components, so `lmn .` would
    // yield `file:///repo/.` and `a/../b` would yield `.../a/../b`. Fold them away *lexically* (no
    // filesystem access, so symlinks aren't resolved and non-existent files still work) for a clean
    // canonical `file://` URI every server accepts.
    let mut out = PathBuf::new();
    for comp in abs.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    if out.as_os_str().is_empty() {
        abs
    } else {
        out
    }
}

/// Bytes read from a file's head to classify it. 8 KiB is what git sniffs, and it is enough
/// for every magic number below plus a representative NUL scan.
const SNIFF_BYTES: usize = 8 * 1024;

/// Leading byte sequences that identify a file as binary, with the label shown to the user.
///
/// Deliberately conservative: every entry is either non-ASCII or a long, distinctive ASCII run,
/// because a match here *refuses* the file. Formats whose signature is short and plausibly
/// text-like (`MZ`, `BM`, `ID3`, `BZh`) are left out — they are caught by the NUL scan instead,
/// where the worst case is a less specific label rather than a rejected text file.
const MAGIC: &[(&[u8], &str)] = &[
    (b"%PDF-", "PDF document"),
    (b"\x89PNG\r\n\x1a\n", "PNG image"),
    (b"\xFF\xD8\xFF", "JPEG image"),
    (b"GIF87a", "GIF image"),
    (b"GIF89a", "GIF image"),
    (b"8BPS", "Photoshop document"),
    (b"\x7FELF", "ELF executable"),
    (b"\xCA\xFE\xBA\xBE", "Java class file"),
    (b"\xFE\xED\xFA\xCE", "Mach-O binary"),
    (b"\xFE\xED\xFA\xCF", "Mach-O binary"),
    (b"\xCE\xFA\xED\xFE", "Mach-O binary"),
    (b"\xCF\xFA\xED\xFE", "Mach-O binary"),
    (b"\x00asm", "WebAssembly module"),
    (b"PK\x03\x04", "ZIP archive"),
    (b"PK\x05\x06", "ZIP archive"),
    (b"PK\x07\x08", "ZIP archive"),
    (b"\x1F\x8B", "gzip archive"),
    (b"\xFD7zXZ\x00", "XZ archive"),
    (b"7z\xBC\xAF\x27\x1C", "7-Zip archive"),
    (b"Rar!\x1A\x07", "RAR archive"),
    (b"\x28\xB5\x2F\xFD", "Zstandard archive"),
    (b"!<arch>", "ar archive"),
    (b"\xED\xAB\xEE\xDB", "RPM package"),
    (b"SQLite format 3\x00", "SQLite database"),
    (b"\xD0\xCF\x11\xE0", "Microsoft Office document"),
    (b"OggS", "Ogg media"),
    (b"fLaC", "FLAC audio"),
    (b"\x1A\x45\xDF\xA3", "Matroska video"),
    (b"OTTO", "OpenType font"),
    (b"wOFF", "WOFF font"),
    (b"wOF2", "WOFF2 font"),
];

/// What a file's head says it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    /// Decodable as text — safe to load into a rope.
    Text,
    /// Not text. `label` names the format when a magic number matched, else it is the generic
    /// "Binary file" (or "Device or pipe" for a non-regular file).
    Binary { label: &'static str },
}

/// What a cheap head-probe learned about a file, without reading it whole.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Probe {
    /// Size in bytes as reported by the filesystem (0 for a non-regular file).
    pub len: u64,
    pub kind: FileKind,
}

/// Why lumina declined to load a file into a text buffer. Carried by the notice tab, which
/// renders it, so this is a *view* of a refusal rather than an error to log and drop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// The bytes aren't text. Not overridable: a NUL-bearing buffer can't round-trip through a
    /// UTF-8 rope, so "open anyway" would corrupt the file on the first save.
    Binary { label: &'static str, len: u64 },
    /// Text, but over the configured ceiling. Overridable (`file.openAnyway`).
    TooLarge { len: u64, limit: u64 },
}

impl Refusal {
    /// The heading shown on the notice tab.
    pub fn label(&self) -> &'static str {
        match self {
            Refusal::Binary { label, .. } => label,
            Refusal::TooLarge { .. } => "Large file",
        }
    }

    /// The file's size in bytes.
    pub fn len(&self) -> u64 {
        match self {
            Refusal::Binary { len, .. } | Refusal::TooLarge { len, .. } => *len,
        }
    }

    /// True when the user can force the file open as text anyway (`file.openAnyway`).
    pub fn is_overridable(&self) -> bool {
        matches!(self, Refusal::TooLarge { .. })
    }
}

/// The size policy applied when opening a file, resolved from user config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Refuse files larger than this. `0` disables the ceiling.
    pub max_bytes: u64,
    /// At or above this, open the file but mark it [`Document::large`] so per-document work
    /// that scales with size (syntax, git, LSP) stays off. `0` disables degraded mode.
    pub large_bytes: u64,
}

impl Limits {
    /// Build from the two megabyte-denominated config keys.
    pub fn from_mb(max_mb: u64, large_mb: u64) -> Limits {
        Limits {
            max_bytes: max_mb.saturating_mul(1024 * 1024),
            large_bytes: large_mb.saturating_mul(1024 * 1024),
        }
    }

    /// Whether a file of `len` bytes should open in degraded mode.
    pub fn is_large(&self, len: u64) -> bool {
        self.large_bytes > 0 && len >= self.large_bytes
    }

    /// Whether a file of `len` bytes is over the ceiling.
    fn is_over(&self, len: u64) -> bool {
        self.max_bytes > 0 && len > self.max_bytes
    }
}

/// The outcome of [`open`]: either a document to put in a tab, or a refusal to explain.
pub enum Opened {
    /// Safe to edit. `large` is already set from the limits.
    Text(Box<Document>),
    Refused(Refusal),
}

/// Classify `path` from its metadata and first [`SNIFF_BYTES`] bytes. O(header), never O(file):
/// this is what keeps `lmn huge.mp4` from spending a gigabyte of RAM to draw one frame.
pub fn probe(path: &Path) -> Result<Probe> {
    let meta = fs::metadata(path).with_context(|| format!("reading {}", path.display()))?;
    // A fifo/device isn't a file we can meaningfully edit, and reading one can block forever
    // (`lmn /dev/urandom`). Classify it from the metadata alone — never open it.
    if !meta.is_file() {
        return Ok(Probe {
            len: 0,
            kind: FileKind::Binary {
                label: "Device or pipe",
            },
        });
    }
    let mut file = fs::File::open(path).with_context(|| format!("reading {}", path.display()))?;
    let mut head = vec![0u8; SNIFF_BYTES];
    let n = read_head(&mut file, &mut head)?;
    head.truncate(n);
    Ok(Probe {
        len: meta.len(),
        kind: classify(&head),
    })
}

/// Fill `buf` from `file`, tolerating short reads, and return how many bytes landed.
fn read_head(file: &mut fs::File, buf: &mut [u8]) -> Result<usize> {
    let mut filled = 0;
    while filled < buf.len() {
        match file.read(&mut buf[filled..])? {
            0 => break,
            n => filled += n,
        }
    }
    Ok(filled)
}

/// Classify a file's head bytes. Ordered so the *specific* answer wins: BOM (which makes NULs
/// legitimate), then magic numbers, then the generic NUL scan.
pub fn classify(head: &[u8]) -> FileKind {
    // UTF-16 is text whose encoding is half NUL bytes — it must be decided before the NUL scan
    // or every UTF-16 file in the project would be refused as binary.
    if head.starts_with(&[0xFF, 0xFE]) || head.starts_with(&[0xFE, 0xFF]) {
        return FileKind::Text;
    }
    for (prefix, label) in MAGIC {
        if head.starts_with(prefix) {
            return FileKind::Binary { label };
        }
    }
    // Container formats whose signature isn't at offset 0.
    if head.starts_with(b"RIFF") && head.len() >= 12 {
        match &head[8..12] {
            b"WEBP" => {
                return FileKind::Binary {
                    label: "WebP image",
                }
            }
            b"WAVE" => return FileKind::Binary { label: "WAV audio" },
            b"AVI " => return FileKind::Binary { label: "AVI video" },
            _ => {}
        }
    }
    if head.len() >= 8 && &head[4..8] == b"ftyp" {
        return FileKind::Binary {
            label: "MP4 / QuickTime media",
        };
    }
    if head.contains(&0) {
        return FileKind::Binary {
            label: "Binary file",
        };
    }
    FileKind::Text
}

/// The single "path → tab content" chokepoint: probe `path`, apply `limits`, and read the whole
/// file **only** once it has passed both. Every route that turns a path into a tab (CLI argument,
/// session restore, explorer, quick-open, project search, LSP navigation, `Host::open_path`)
/// goes through here, so the policy can't be bypassed by adding a caller.
pub fn open(path: &Path, limits: &Limits) -> Result<Opened> {
    let probe = probe(path)?;
    if let FileKind::Binary { label } = probe.kind {
        return Ok(Opened::Refused(Refusal::Binary {
            label,
            len: probe.len,
        }));
    }
    if limits.is_over(probe.len) {
        return Ok(Opened::Refused(Refusal::TooLarge {
            len: probe.len,
            limit: limits.max_bytes,
        }));
    }
    Ok(Opened::Text(Box::new(load_sized(path, limits)?)))
}

/// Load `path` past the size ceiling (the `file.openAnyway` escape hatch).
///
/// A file forced open this way is **always** in degraded mode, even when it sits under
/// `large_bytes`: the user has already been told it is over the ceiling, and re-enabling a
/// tree-sitter parse plus a full-buffer `didOpen` on the file they were just warned about is
/// the opposite of what "open anyway" asks for.
pub fn open_forced(path: &Path, limits: &Limits) -> Result<Document> {
    let mut doc = load_sized(path, limits)?;
    let len = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    doc.large |= limits.is_over(len);
    Ok(doc)
}

/// [`load`], plus the degraded-mode flag derived from the file's size.
fn load_sized(path: &Path, limits: &Limits) -> Result<Document> {
    let mut doc = load(path)?;
    let len = fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    doc.large = limits.is_large(len);
    Ok(doc)
}

/// Format a byte count for display: `842 B`, `12.4 KB`, `1.9 GB`. Base-1024 with the customary
/// short units, one decimal place above a kilobyte.
pub fn human_bytes(n: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = n as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{n} B")
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
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

    /// Write `bytes` to a uniquely-named temp file and hand back the path.
    fn temp_bytes(tag: &str, bytes: &[u8]) -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let p = std::env::temp_dir().join(format!(
            "lumina_probe_{}_{}_{tag}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        fs::write(&p, bytes).unwrap();
        p
    }

    #[test]
    fn classify_labels_every_magic_number() {
        // Exhaustive over the table, so a new entry can't be added without a label check.
        for (prefix, label) in MAGIC {
            let mut head = prefix.to_vec();
            head.extend_from_slice(b"trailing bytes");
            assert_eq!(
                classify(&head),
                FileKind::Binary { label },
                "{label} should be detected from its magic number"
            );
        }
        // The container formats whose signature isn't at offset 0.
        assert_eq!(
            classify(b"RIFF\x00\x00\x00\x00WEBPmore"),
            FileKind::Binary {
                label: "WebP image"
            }
        );
        assert_eq!(
            classify(b"\x00\x00\x00\x18ftypmp42"),
            FileKind::Binary {
                label: "MP4 / QuickTime media"
            }
        );
    }

    #[test]
    fn classify_falls_back_to_a_nul_scan() {
        assert_eq!(
            classify(b"some text\0with a nul"),
            FileKind::Binary {
                label: "Binary file"
            }
        );
    }

    #[test]
    fn utf16_is_text_despite_its_nul_bytes() {
        // The whole point of ordering the BOM check first: half of a UTF-16 file's bytes are
        // NUL, so a naive binary sniff would refuse every one of them.
        let le = encode_utf16("hello", false);
        assert_eq!(classify(&le), FileKind::Text);
        let be = encode_utf16("hello", true);
        assert_eq!(classify(&be), FileKind::Text);
    }

    #[test]
    fn ordinary_text_and_edge_cases_are_text() {
        assert_eq!(classify(b"fn main() {}\n"), FileKind::Text);
        assert_eq!(classify(b""), FileKind::Text, "an empty file is text");
        // Shorter than the longest magic prefix — must compare on the available slice, not panic.
        assert_eq!(classify(b"%P"), FileKind::Text);
        // Latin-1 source (invalid UTF-8, no NULs) still opens, lossily, as it always has.
        assert_eq!(classify(b"caf\xe9 au lait"), FileKind::Text);
    }

    #[test]
    fn probe_reports_size_and_kind_without_reading_the_body() {
        let mut pdf = b"%PDF-1.7\n".to_vec();
        pdf.extend(std::iter::repeat_n(b'x', 100_000));
        let path = temp_bytes("doc.pdf", &pdf);
        let probe = probe(&path).unwrap();
        fs::remove_file(&path).ok();
        assert_eq!(probe.len, pdf.len() as u64);
        assert_eq!(
            probe.kind,
            FileKind::Binary {
                label: "PDF document"
            }
        );
    }

    #[test]
    fn open_refuses_binary_and_over_size_and_admits_text() {
        let limits = Limits::from_mb(1, 1);

        let pdf = temp_bytes("a.pdf", b"%PDF-1.7\nbody");
        assert!(matches!(
            open(&pdf, &limits),
            Ok(Opened::Refused(Refusal::Binary {
                label: "PDF document",
                ..
            }))
        ));
        fs::remove_file(&pdf).ok();

        let big = temp_bytes("big.txt", &vec![b'a'; 2 * 1024 * 1024]);
        match open(&big, &limits) {
            Ok(Opened::Refused(Refusal::TooLarge { len, limit })) => {
                assert_eq!(len, 2 * 1024 * 1024);
                assert_eq!(limit, 1024 * 1024);
            }
            other => panic!("expected TooLarge, got {:?}", other.map(|_| "…")),
        }
        fs::remove_file(&big).ok();

        let small = temp_bytes("small.txt", b"hello\n");
        match open(&small, &limits) {
            Ok(Opened::Text(doc)) => {
                assert_eq!(doc.to_string(), "hello\n");
                assert!(!doc.large, "a 6-byte file is not large");
            }
            _ => panic!("a small text file should open"),
        }
        fs::remove_file(&small).ok();
    }

    #[test]
    fn a_file_between_the_thresholds_opens_in_degraded_mode() {
        // 2 MB with a 1 MB "large" threshold and a 16 MB ceiling: opens, but flagged so syntax,
        // git, and LSP stay off.
        let limits = Limits::from_mb(16, 1);
        let path = temp_bytes("mid.txt", &vec![b'a'; 2 * 1024 * 1024]);
        match open(&path, &limits) {
            Ok(Opened::Text(doc)) => assert!(doc.large, "should be flagged large"),
            _ => panic!("should open"),
        }
        // The override path keeps the flag too — forcing past the ceiling must not also turn
        // the expensive per-document features back on.
        let forced = open_forced(&path, &Limits::from_mb(1, 1)).unwrap();
        assert!(forced.large);
        fs::remove_file(&path).ok();
    }

    #[test]
    fn a_zero_limit_disables_the_ceiling() {
        let limits = Limits::from_mb(0, 0);
        let path = temp_bytes("any.txt", &vec![b'a'; 4096]);
        assert!(matches!(open(&path, &limits), Ok(Opened::Text(_))));
        assert!(!limits.is_large(u64::MAX));
        fs::remove_file(&path).ok();
    }

    #[test]
    fn a_directory_or_missing_file_surfaces_the_io_error() {
        assert!(probe(Path::new("/definitely/not/here")).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn a_device_is_classified_without_being_opened() {
        // Reading /dev/urandom would never return; the probe must decide from metadata alone.
        let probe = probe(Path::new("/dev/urandom")).unwrap();
        assert_eq!(
            probe.kind,
            FileKind::Binary {
                label: "Device or pipe"
            }
        );
    }

    #[test]
    fn human_bytes_scales_units() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(842), "842 B");
        assert_eq!(human_bytes(1024), "1.0 KB");
        assert_eq!(human_bytes(1024 * 1024 * 3 / 2), "1.5 MB");
        assert_eq!(human_bytes(5 * 1024 * 1024 * 1024), "5.0 GB");
    }

    #[test]
    fn refusal_reports_its_own_shape() {
        let binary = Refusal::Binary {
            label: "PDF document",
            len: 10,
        };
        assert_eq!(binary.label(), "PDF document");
        assert_eq!(binary.len(), 10);
        // Not overridable: forcing binary through the UTF-8 rope would corrupt it on save.
        assert!(!binary.is_overridable());

        let large = Refusal::TooLarge { len: 99, limit: 10 };
        assert_eq!(large.label(), "Large file");
        assert!(large.is_overridable());
    }

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
        use std::path::Component;
        let abs = absolute_path(Path::new("crates/app/src/app.rs"));
        assert!(abs.is_absolute(), "relative paths are absolutized");
        // The exact `file://` form is platform-specific (Windows absolute paths are `C:\…`); the
        // `file:///…` shape is the Unix one that servers on the user's platform expect.
        #[cfg(unix)]
        {
            let uri = crate::lsp::uri_for(&abs);
            assert!(uri.starts_with("file:///"), "well-formed file URI: {uri}");
        }
        // `.`/`..` are folded away so every relative launch form yields a clean canonical path.
        let normed = absolute_path(Path::new("crates/../app/./src/x.rs"));
        assert!(
            !normed
                .components()
                .any(|c| matches!(c, Component::CurDir | Component::ParentDir)),
            "no . or .. components remain: {normed:?}"
        );
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
