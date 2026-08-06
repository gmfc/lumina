//! Turning a PDF's bytes into readable text: object discovery, the page tree, and the
//! content-stream text operators.
//!
//! **Objects are found by scanning, not by the cross-reference table.** An xref walk is the
//! textbook approach and the wrong one here: the files that reach an editor are the damaged,
//! hand-edited, and incrementally-updated ones, whose xref tables are exactly what went stale.
//! Scanning for `N G obj … endobj` reads all of them. Object streams (`/Type /ObjStm`) are
//! expanded afterwards, because PDF 1.5+ producers put page dictionaries inside them and a
//! scanner that ignored them would find no pages at all in most modern documents.

use std::collections::HashMap;

use super::parse::{decode_stream, find, get, Dict, Lexer, Obj};

/// Objects we will hold at once. A bound, not a budget — real documents are far under it, and
/// a hostile file that declares millions of objects stops here instead of exhausting memory.
const MAX_OBJECTS: usize = 500_000;

/// Decoded content bytes we will accumulate for a single page.
const MAX_PAGE_CONTENT: usize = 64 * 1024 * 1024;

/// Total nodes the `/Pages` → `/Kids` walk may visit. The depth cap bounds one path; this
/// bounds the whole traversal, which is what a branching or self-referential tree needs.
const MAX_TREE_VISITS: usize = 100_000;

/// Depth cap for the `/Pages` → `/Kids` walk, so a cyclic or absurd page tree terminates.
const MAX_TREE_DEPTH: usize = 32;

/// A parsed PDF: every object we could find, plus what the trailer told us.
pub(super) struct Pdf {
    objects: HashMap<u32, Obj>,
    /// True when the document declares `/Encrypt` — strings and streams are then ciphertext,
    /// and extraction would produce noise rather than text.
    pub(super) encrypted: bool,
    /// The `/Info` dictionary's `(key, value)` pairs, when present.
    pub(super) info: Vec<(String, String)>,
}

impl Pdf {
    pub(super) fn parse(bytes: &[u8]) -> Pdf {
        let mut objects = scan_objects(bytes);
        expand_object_streams(&mut objects);
        let trailers = scan_trailers(bytes, &objects);
        let encrypted = trailers.iter().any(|d| get(d, "Encrypt").is_some());
        let info = trailers
            .iter()
            .find_map(|d| get(d, "Info"))
            .and_then(|r| resolve(&objects, r).and_then(Obj::dict).cloned())
            .map(|d| info_pairs(&objects, &d))
            .unwrap_or_default();
        Pdf {
            objects,
            encrypted,
            info,
        }
    }

    pub(super) fn object_count(&self) -> usize {
        self.objects.len()
    }

    /// Follow an indirect reference to the object it names (one hop is all PDF allows).
    fn resolve<'a>(&'a self, obj: &'a Obj) -> Option<&'a Obj> {
        resolve(&self.objects, obj)
    }

    /// Look up `key` in `dict`, resolving an indirect value.
    fn lookup<'a>(&'a self, dict: &'a Dict, key: &str) -> Option<&'a Obj> {
        self.resolve(get(dict, key)?)
    }

    /// Every page's dictionary, in document order.
    ///
    /// Preferred route: catalog → `/Pages` → `/Kids`, which is the only thing that gives the
    /// *right* order. Fallback: every `/Type /Page` object sorted by object number, which is
    /// usually document order and is always better than reporting an empty document.
    pub(super) fn pages(&self) -> Vec<&Dict> {
        let root = self
            .objects
            .values()
            .find(|o| o.get("Type").and_then(Obj::as_name) == Some("Catalog"))
            .and_then(|cat| self.lookup(cat.dict()?, "Pages"))
            .and_then(Obj::dict);
        if let Some(pages) = root {
            let mut out = Vec::new();
            let mut budget = MAX_TREE_VISITS;
            self.walk_pages(pages, 0, &mut out, &mut budget);
            if !out.is_empty() {
                return out;
            }
        }
        let mut numbered: Vec<(u32, &Dict)> = self
            .objects
            .iter()
            .filter(|(_, o)| o.get("Type").and_then(Obj::as_name) == Some("Page"))
            .filter_map(|(n, o)| Some((*n, o.dict()?)))
            .collect();
        numbered.sort_by_key(|(n, _)| *n);
        numbered.into_iter().map(|(_, d)| d).collect()
    }

    /// `budget` bounds *total node visits*, which the depth cap alone does not: a node whose
    /// `/Kids` names itself, or a chain of `/Kids [n n]` branches, visits 2^depth nodes without
    /// ever reaching a leaf — so `out` never grows and only the depth cap applies, at 2^32
    /// visits. That is a hang on a damaged or hostile file, not a slow parse.
    fn walk_pages<'a>(
        &'a self,
        node: &'a Dict,
        depth: usize,
        out: &mut Vec<&'a Dict>,
        budget: &mut usize,
    ) {
        if depth > MAX_TREE_DEPTH || out.len() >= MAX_OBJECTS || *budget == 0 {
            return;
        }
        *budget -= 1;
        let Some(kids) = self.lookup(node, "Kids").and_then(Obj::as_array) else {
            // A leaf reached directly (a one-page document sometimes has no intermediate node).
            if get(node, "Type").and_then(Obj::as_name) == Some("Page") {
                out.push(node);
            }
            return;
        };
        for kid in kids {
            let Some(kid) = self.resolve(kid).and_then(Obj::dict) else {
                continue;
            };
            match get(kid, "Type").and_then(Obj::as_name) {
                Some("Pages") => self.walk_pages(kid, depth + 1, out, budget),
                // Missing `/Type` is common in generated files; a node with `/Kids` is a branch,
                // anything else is a leaf.
                Some("Page") => out.push(kid),
                _ if get(kid, "Kids").is_some() => self.walk_pages(kid, depth + 1, out, budget),
                _ => out.push(kid),
            }
        }
    }

    /// The decoded bytes of a page's content stream(s), concatenated.
    fn page_content(&self, page: &Dict) -> Vec<u8> {
        let mut out = Vec::new();
        let Some(contents) = self.lookup(page, "Contents") else {
            return out;
        };
        // `/Contents` is either one stream or an array of them, which together form a single
        // stream — a text object may even straddle the boundary, so they must be concatenated
        // before parsing rather than parsed separately.
        let parts: Vec<&Obj> = match contents {
            Obj::Array(items) => items.iter().filter_map(|i| self.resolve(i)).collect(),
            other => vec![other],
        };
        for part in parts {
            // `/Contents` may name the same stream hundreds of times; each entry is decoded
            // again, so a 3 MB stream referenced 300× is a gigabyte of work from a small file.
            if out.len() >= MAX_PAGE_CONTENT {
                break;
            }
            if let Obj::Stream { dict, data } = part {
                if let Some(decoded) = decode_stream(dict, data) {
                    out.extend_from_slice(&decoded);
                    out.push(b'\n');
                }
            }
        }
        out
    }

    /// The `/Font` resources visible to a page, walking up `/Parent` for inherited resources.
    fn page_fonts(&self, page: &Dict) -> HashMap<String, CMap> {
        let mut fonts = HashMap::new();
        let mut node = page;
        for _ in 0..MAX_TREE_DEPTH {
            if let Some(dict) = self
                .lookup(node, "Resources")
                .and_then(Obj::dict)
                .and_then(|r| self.lookup(r, "Font"))
                .and_then(Obj::dict)
            {
                for (name, font_ref) in dict {
                    if fonts.contains_key(name) {
                        continue; // the nearest definition wins
                    }
                    let Some(font) = self.resolve(font_ref).and_then(Obj::dict) else {
                        continue;
                    };
                    if let Some(cmap) = self.to_unicode(font) {
                        fonts.insert(name.clone(), cmap);
                    }
                }
            }
            match self.lookup(node, "Parent").and_then(Obj::dict) {
                Some(parent) => node = parent,
                None => break,
            }
        }
        fonts
    }

    /// A font's `/ToUnicode` CMap, which is the only reliable way to read text set in a subset
    /// embedded font (whose codes are arbitrary glyph indices, not characters).
    fn to_unicode(&self, font: &Dict) -> Option<CMap> {
        let obj = self.lookup(font, "ToUnicode")?;
        let Obj::Stream { dict, data } = obj else {
            return None;
        };
        Some(CMap::parse(&decode_stream(dict, data)?))
    }

    /// Extract a page's text.
    pub(super) fn page_text(&self, page: &Dict) -> String {
        let content = self.page_content(page);
        let fonts = self.page_fonts(page);
        extract_text(&content, &fonts)
    }
}

fn resolve<'a>(objects: &'a HashMap<u32, Obj>, obj: &'a Obj) -> Option<&'a Obj> {
    match obj {
        Obj::Ref(n) => objects.get(n),
        other => Some(other),
    }
}

/// Flatten an `/Info` dictionary into displayable `(key, value)` pairs, dropping the binary and
/// structural entries nobody wants to read.
fn info_pairs(objects: &HashMap<u32, Obj>, info: &Dict) -> Vec<(String, String)> {
    const KEYS: &[&str] = &[
        "Title",
        "Author",
        "Subject",
        "Keywords",
        "Creator",
        "Producer",
        "CreationDate",
        "ModDate",
    ];
    KEYS.iter()
        .filter_map(|key| {
            let value = resolve(objects, get(info, key)?)?;
            let text = match value {
                Obj::Str(bytes) => decode_text_string(bytes),
                Obj::Name(n) => n.clone(),
                _ => return None,
            };
            let text = text.trim().to_string();
            (!text.is_empty()).then_some(((*key).to_string(), text))
        })
        .collect()
}

/// Decode a PDF "text string": UTF-16BE when it carries the BOM, else PDFDocEncoding (which
/// agrees with Latin-1 over the range that matters here).
fn decode_text_string(bytes: &[u8]) -> String {
    if bytes.starts_with(&[0xFE, 0xFF]) {
        let units: Vec<u16> = bytes[2..]
            .chunks_exact(2)
            .map(|c| u16::from_be_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    } else {
        bytes.iter().map(|&b| win_ansi(b)).collect()
    }
}

/// Find every `N G obj … endobj` in the file.
///
/// The scan skips each object's parsed extent, so the `obj` bytes that inevitably occur inside
/// a compressed stream are never mistaken for an object header.
fn scan_objects(buf: &[u8]) -> HashMap<u32, Obj> {
    let mut objects = HashMap::new();
    let mut i = 0usize;
    while i + 3 <= buf.len() && objects.len() < MAX_OBJECTS {
        if &buf[i..i + 3] != b"obj" {
            i += 1;
            continue;
        }
        // `endobj` also contains "obj"; a header's keyword is preceded by whitespace.
        let Some(num) = object_number_before(buf, i) else {
            i += 3;
            continue;
        };
        let mut lex = Lexer::at(buf, i + 3);
        let obj = lex.object();
        i = lex.pos.max(i + 3);
        if let Some(obj) = obj {
            // Later definitions win: that is what an incremental update means.
            objects.insert(num, obj);
        }
    }
    objects
}

/// Read the `N G` that must precede an `obj` keyword at `at`, returning `N`.
fn object_number_before(buf: &[u8], at: usize) -> Option<u32> {
    let digits_back = |mut end: usize| -> Option<(u32, usize)> {
        let stop = end;
        while end > 0 && buf[end - 1].is_ascii_digit() {
            end -= 1;
        }
        if end == stop {
            return None;
        }
        let text = std::str::from_utf8(&buf[end..stop]).ok()?;
        Some((text.parse().ok()?, end))
    };
    let ws_back = |mut end: usize| -> usize {
        while end > 0 && matches!(buf[end - 1], b' ' | b'\t' | b'\r' | b'\n' | b'\0' | 0x0c) {
            end -= 1;
        }
        end
    };
    let after_ws = ws_back(at);
    if after_ws == at {
        return None; // `obj` must be preceded by whitespace
    }
    let (_gen, before_gen) = digits_back(after_ws)?;
    let after_ws2 = ws_back(before_gen);
    if after_ws2 == before_gen {
        return None;
    }
    let (num, before_num) = digits_back(after_ws2)?;
    // The object number must itself start a token.
    let ok = before_num == 0 || !buf[before_num - 1].is_ascii_digit();
    ok.then_some(num)
}

/// Expand `/Type /ObjStm` containers, whose payload is `N` objects preceded by a header of
/// `objnum offset` pairs. PDF 1.5+ writers put most of the document — including page
/// dictionaries — in these, so this is not an optional refinement.
fn expand_object_streams(objects: &mut HashMap<u32, Obj>) {
    let streams: Vec<(Dict, Vec<u8>)> = objects
        .values()
        .filter_map(|o| match o {
            Obj::Stream { dict, data }
                if get(dict, "Type").and_then(Obj::as_name) == Some("ObjStm") =>
            {
                Some((dict.clone(), data.clone()))
            }
            _ => None,
        })
        .collect();
    for (dict, data) in streams {
        let Some(decoded) = decode_stream(&dict, &data) else {
            continue;
        };
        let count = get(&dict, "N").and_then(Obj::as_f64).unwrap_or(0.0) as usize;
        let first = get(&dict, "First").and_then(Obj::as_f64).unwrap_or(0.0) as usize;
        if first > decoded.len() {
            continue;
        }
        let mut header = Lexer::new(&decoded[..first]);
        for _ in 0..count.min(MAX_OBJECTS) {
            let (Some(num), Some(offset)) = (
                header.object().and_then(|o| o.as_f64()),
                header.object().and_then(|o| o.as_f64()),
            ) else {
                break;
            };
            if num < 0.0 || offset < 0.0 {
                break;
            }
            // `offset` is a file-supplied float: `1e30 as usize` saturates to `usize::MAX`, so
            // a plain add panics in debug and wraps in release.
            let Some(at) = first.checked_add(offset as usize) else {
                continue;
            };
            if at >= decoded.len() {
                continue;
            }
            if let Some(obj) = Lexer::at(&decoded, at).object() {
                // Containers never override a top-level definition: a later incremental update
                // writes the object directly, and that copy is the current one.
                objects.entry(num as u32).or_insert(obj);
            }
        }
    }
}

/// Trailer-shaped dictionaries: the classic `trailer << … >>` and the PDF 1.5 `/Type /XRef`
/// stream dictionary that replaced it. These are where `/Encrypt` and `/Info` live.
fn scan_trailers(buf: &[u8], objects: &HashMap<u32, Obj>) -> Vec<Dict> {
    let mut out: Vec<Dict> = objects
        .values()
        .filter(|o| o.get("Type").and_then(Obj::as_name) == Some("XRef"))
        .filter_map(|o| o.dict().cloned())
        .collect();
    let mut at = 0usize;
    while let Some(rel) = find(&buf[at..], b"trailer") {
        let pos = at + rel + b"trailer".len();
        if let Some(Obj::Dict(d)) = Lexer::at(buf, pos).object() {
            out.push(d);
        }
        at = pos;
    }
    out
}

// --- content-stream text extraction -----------------------------------------------------

/// A `/ToUnicode` CMap: source codes → replacement text, plus how many bytes a code occupies.
struct CMap {
    map: HashMap<u32, String>,
    code_bytes: usize,
}

impl CMap {
    /// Parse the `beginbfchar`/`beginbfrange` sections. The rest of the CMap grammar (codespace
    /// ranges, usecmap) only refines what we already infer from the entries themselves.
    fn parse(data: &[u8]) -> CMap {
        /// Entry cap. A real `/ToUnicode` CMap holds at most a few thousand mappings; a file
        /// declaring range after 64 K-wide range would otherwise build an arbitrarily large map
        /// on the UI thread.
        const MAX_ENTRIES: usize = 65_536;

        let mut map: HashMap<u32, String> = HashMap::new();
        let mut code_bytes = 1usize;
        let mut lex = Lexer::new(data);
        let mut pending: Vec<Obj> = Vec::new();
        loop {
            if let Some(obj) = lex.object() {
                if pending.len() < 4096 {
                    pending.push(obj);
                }
                continue;
            }
            let Some(word) = lex.keyword() else {
                if lex.pos >= data.len() {
                    break;
                }
                lex.pos += 1;
                continue;
            };
            match word {
                b"endbfchar" => {
                    for pair in pending.chunks(2) {
                        let [Obj::Str(src), Obj::Str(dst)] = pair else {
                            continue;
                        };
                        if map.len() >= MAX_ENTRIES {
                            break;
                        }
                        code_bytes = code_bytes.max(src.len().clamp(1, 4));
                        map.insert(be_code(src), decode_utf16be(dst));
                    }
                }
                b"endbfrange" => {
                    for triple in pending.chunks(3) {
                        let [Obj::Str(lo), Obj::Str(hi), dst] = triple else {
                            continue;
                        };
                        if map.len() >= MAX_ENTRIES {
                            break;
                        }
                        code_bytes = code_bytes.max(lo.len().clamp(1, 4));
                        let (lo, hi) = (be_code(lo), be_code(hi));
                        // A range of 2^32 would hang; real ranges are tiny.
                        if hi < lo || hi - lo > 0xFFFF {
                            continue;
                        }
                        match dst {
                            // `<lo> <hi> <dstStart>`: consecutive codes map to consecutive chars.
                            Obj::Str(start) => {
                                let base = decode_utf16be(start);
                                let Some(first) = base.chars().next() else {
                                    continue;
                                };
                                let prefix: String = base.chars().skip(1).collect();
                                for code in lo..=hi {
                                    let ch = char::from_u32(first as u32 + (code - lo))
                                        .unwrap_or(char::REPLACEMENT_CHARACTER);
                                    map.insert(code, format!("{ch}{prefix}"));
                                }
                            }
                            // `<lo> <hi> [<dst> <dst> …]`: one destination per code.
                            Obj::Array(items) => {
                                for (i, item) in items.iter().enumerate() {
                                    // `lo` comes from the file, so the destination code can run
                                    // past u32 — a debug-build panic on a malformed CMap.
                                    let Some(code) = lo.checked_add(i as u32) else {
                                        break;
                                    };
                                    if let Obj::Str(s) = item {
                                        map.insert(code, decode_utf16be(s));
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
            if matches!(
                word,
                b"beginbfchar" | b"endbfchar" | b"beginbfrange" | b"endbfrange"
            ) {
                pending.clear();
            }
        }
        CMap { map, code_bytes }
    }

    /// Decode a show-string's bytes through this CMap.
    fn decode(&self, bytes: &[u8]) -> String {
        let width = self.code_bytes.clamp(1, 4);
        let mut out = String::new();
        for chunk in bytes.chunks(width) {
            let code = be_code(chunk);
            match self.map.get(&code) {
                Some(text) => out.push_str(text),
                // An unmapped code in a 1-byte font is almost always plain text the CMap simply
                // didn't cover; in a 2-byte font it is a glyph id with no textual meaning.
                None if width == 1 => out.push(win_ansi(code as u8)),
                None => {}
            }
        }
        out
    }
}

/// Interpret up to 4 big-endian bytes as a code.
fn be_code(bytes: &[u8]) -> u32 {
    bytes
        .iter()
        .take(4)
        .fold(0u32, |acc, &b| (acc << 8) | b as u32)
}

fn decode_utf16be(bytes: &[u8]) -> String {
    let units: Vec<u16> = bytes
        .chunks_exact(2)
        .map(|c| u16::from_be_bytes([c[0], c[1]]))
        .collect();
    if units.is_empty() {
        return bytes.iter().map(|&b| win_ansi(b)).collect();
    }
    String::from_utf16_lossy(&units)
}

/// WinAnsiEncoding (≈ Latin-1, but 0x80–0x9F carries printable characters rather than
/// controls). The default for simple fonts, which is what most text PDFs use.
fn win_ansi(b: u8) -> char {
    const HIGH: [char; 32] = [
        '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8d}', 'Ž',
        '\u{8f}', '\u{90}', '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ', '\u{9d}',
        'ž', 'Ÿ',
    ];
    match b {
        0x80..=0x9F => HIGH[(b - 0x80) as usize],
        _ => b as char,
    }
}

/// A `TJ` kerning adjustment more negative than this reads as a word space. The unit is
/// thousandths of an em, and typical inter-word kerning in generated PDFs is around -250.
const TJ_SPACE: f64 = 100.0;

/// Vertical movement (in text space) that counts as a new line rather than a baseline nudge
/// for a superscript.
const LINE_EPSILON: f64 = 0.6;

/// Run a content stream's text operators and collect what they draw.
///
/// This is an interpreter for the text-showing subset of the operator set: enough to recover
/// reading order and line breaks, not a renderer. Positioning is tracked only as far as it
/// answers "did this string start a new line?".
fn extract_text(content: &[u8], fonts: &HashMap<String, CMap>) -> String {
    let mut lex = Lexer::new(content);
    let mut operands: Vec<Obj> = Vec::new();
    let mut out = String::new();
    let mut font: Option<&CMap> = None;
    let mut leading = 0.0f64;
    let mut line_y = 0.0f64;
    let mut last_y: Option<f64> = None;

    // Emit a line break when the baseline moved since the last shown string.
    macro_rules! newline_if_moved {
        () => {
            if let Some(prev) = last_y {
                if (line_y - prev).abs() > LINE_EPSILON {
                    out.push('\n');
                }
            }
            last_y = Some(line_y);
        };
    }

    loop {
        if let Some(obj) = lex.object() {
            // Operand lists are tiny; a pathological stream that pushes without ever issuing an
            // operator must not grow this without bound.
            if operands.len() < 4096 {
                operands.push(obj);
            }
            continue;
        }
        let Some(op) = lex.keyword() else {
            if lex.pos >= content.len() {
                break;
            }
            lex.pos += 1; // an unparseable delimiter — step over it and keep going
            continue;
        };
        let num = |i: usize| -> f64 {
            operands
                .get(operands.len().wrapping_sub(i))
                .and_then(Obj::as_f64)
                .unwrap_or(0.0)
        };
        match op {
            b"BT" => {
                line_y = 0.0;
                last_y = None;
            }
            b"ET" => out.push('\n'),
            b"Tf" => {
                font = operands
                    .iter()
                    .rev()
                    .find_map(Obj::as_name)
                    .and_then(|name| fonts.get(name));
            }
            b"TL" => leading = num(1),
            b"Td" => line_y += num(1),
            b"TD" => {
                leading = -num(1);
                line_y += num(1);
            }
            // `Tm a b c d e f` sets the line matrix outright; `f` is the baseline.
            b"Tm" => line_y = num(1),
            b"T*" => line_y -= leading,
            b"Tj" => {
                newline_if_moved!();
                if let Some(Obj::Str(s)) = operands.last() {
                    out.push_str(&show(s, font));
                }
            }
            b"'" | b"\"" => {
                line_y -= leading;
                newline_if_moved!();
                if let Some(Obj::Str(s)) = operands.last() {
                    out.push_str(&show(s, font));
                }
            }
            b"TJ" => {
                newline_if_moved!();
                if let Some(Obj::Array(items)) = operands.last() {
                    for item in items {
                        match item {
                            Obj::Str(s) => out.push_str(&show(s, font)),
                            // A big negative kern is how PDFs write a space they didn't encode.
                            Obj::Number(n) if *n < -TJ_SPACE => out.push(' '),
                            _ => {}
                        }
                    }
                }
            }
            _ => {}
        }
        operands.clear();
    }
    out
}

/// Decode one show-string through the current font's CMap, or WinAnsi when it has none.
fn show(bytes: &[u8], font: Option<&CMap>) -> String {
    match font {
        Some(cmap) => cmap.decode(bytes),
        None => bytes.iter().map(|&b| win_ansi(b)).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmap_from(entries: &[(u16, &str)]) -> CMap {
        let mut src = String::from("1 beginbfchar\n");
        for (code, text) in entries {
            let utf16: String = text.encode_utf16().map(|u| format!("{:04X}", u)).collect();
            src.push_str(&format!("<{code:04X}> <{utf16}>\n"));
        }
        src.push_str("endbfchar\n");
        CMap::parse(src.as_bytes())
    }

    #[test]
    fn scans_objects_and_ignores_endobj() {
        let src = b"1 0 obj\n<< /Type /Page >>\nendobj\n2 0 obj\n42\nendobj\n";
        let objects = scan_objects(src);
        assert_eq!(objects.len(), 2);
        assert_eq!(objects[&1].get("Type").and_then(Obj::as_name), Some("Page"));
        assert_eq!(objects[&2].as_f64(), Some(42.0));
    }

    #[test]
    fn obj_bytes_inside_a_stream_are_not_mistaken_for_a_header() {
        // The scanner skips each object's extent precisely so this can't happen.
        let src = b"1 0 obj\n<< /Length 12 >>\nstream\n9 0 obj junk\nendstream\nendobj\n";
        let objects = scan_objects(src);
        assert_eq!(objects.len(), 1, "only the real object: {objects:?}");
        assert!(objects.contains_key(&1));
    }

    #[test]
    fn a_later_definition_wins() {
        // That is what an incremental update means.
        let src = b"1 0 obj\n(old)\nendobj\n1 0 obj\n(new)\nendobj\n";
        assert_eq!(scan_objects(src)[&1], Obj::Str(b"new".to_vec()));
    }

    #[test]
    fn cmap_bfchar_maps_codes() {
        let cmap = cmap_from(&[(1, "H"), (2, "i")]);
        assert_eq!(cmap.decode(&[0x00, 0x01, 0x00, 0x02]), "Hi");
    }

    #[test]
    fn cmap_bfrange_maps_consecutive_codes() {
        let src = b"1 beginbfrange\n<0041> <0043> <0061>\nendbfrange\n";
        let cmap = CMap::parse(src);
        assert_eq!(cmap.decode(&[0x00, 0x41, 0x00, 0x42, 0x00, 0x43]), "abc");
    }

    #[test]
    fn cmap_bfrange_array_form_maps_each_code() {
        let src = b"1 beginbfrange\n<0001> <0002> [<0058> <0059>]\nendbfrange\n";
        let cmap = CMap::parse(src);
        assert_eq!(cmap.decode(&[0x00, 0x01, 0x00, 0x02]), "XY");
    }

    #[test]
    fn text_operators_recover_words_and_lines() {
        let content = b"BT /F1 12 Tf 100 700 Td (Hello) Tj 0 -14 Td (World) Tj ET";
        let text = extract_text(content, &HashMap::new());
        assert!(text.contains("Hello"), "{text:?}");
        assert!(text.contains("World"), "{text:?}");
        assert!(
            text.find("Hello").unwrap() < text.find('\n').unwrap(),
            "the baseline move became a line break: {text:?}"
        );
    }

    #[test]
    fn tj_arrays_turn_large_kerns_into_spaces() {
        let content = b"BT [(Hello) -300 (World)] TJ ET";
        assert!(extract_text(content, &HashMap::new()).contains("Hello World"));
        // A small kern is just tracking, not a word break.
        let tight = b"BT [(Hel) -20 (lo)] TJ ET";
        assert!(extract_text(tight, &HashMap::new()).contains("Hello"));
    }

    #[test]
    fn quote_operators_advance_a_line_and_show() {
        let content = b"BT 14 TL (one) ' (two) ' ET";
        let text = extract_text(content, &HashMap::new());
        assert!(text.contains("one") && text.contains("two"), "{text:?}");
        assert!(text.contains('\n'), "each ' starts a new line: {text:?}");
    }

    #[test]
    fn text_uses_the_font_cmap_when_one_is_set() {
        let mut fonts = HashMap::new();
        fonts.insert("F1".to_string(), cmap_from(&[(1, "H"), (2, "i")]));
        let content = b"BT /F1 12 Tf <00010002> Tj ET";
        assert!(extract_text(content, &fonts).contains("Hi"));
    }

    #[test]
    fn winansi_maps_the_0x80_range_to_printable_characters() {
        assert_eq!(win_ansi(0x80), '€');
        assert_eq!(win_ansi(0x92), '’');
        assert_eq!(win_ansi(b'A'), 'A');
    }

    #[test]
    fn a_cmap_cannot_be_made_to_grow_without_bound() {
        // Range after 64 K-wide range: the entry cap is what keeps a hostile CMap from
        // building an arbitrarily large map on the UI thread.
        let mut src = String::new();
        for _ in 0..8 {
            src.push_str("1 beginbfrange\n<0000> <FFFF> <0041>\nendbfrange\n");
        }
        let start = std::time::Instant::now();
        let cmap = CMap::parse(src.as_bytes());
        assert!(cmap.map.len() <= 65_536, "entry cap held");
        assert!(start.elapsed() < std::time::Duration::from_secs(5));
    }

    #[test]
    fn a_bfrange_whose_codes_run_past_u32_stops_instead_of_overflowing() {
        // `lo` comes straight from the file; without a checked add this panics in debug.
        let src = b"1 beginbfrange\n<FFFFFFFF> <FFFFFFFF> [<0041> <0042> <0043>]\nendbfrange\n";
        let cmap = CMap::parse(src);
        assert_eq!(cmap.decode(&[0xFF, 0xFF, 0xFF, 0xFF]), "A");
    }

    #[test]
    fn a_self_referential_page_tree_terminates() {
        // `/Kids` naming its own node never reaches a leaf, so `out` never grows and only the
        // depth cap applies — at 2^32 visits, which is a hang, not a slow parse.
        let mut src = Vec::from(&b"%PDF-1.4\n"[..]);
        src.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        src.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [2 0 R 2 0 R] >>\nendobj\n");
        let pdf = Pdf::parse(&src);
        let start = std::time::Instant::now();
        let pages = pdf.pages();
        assert!(
            start.elapsed() < std::time::Duration::from_secs(2),
            "cyclic page tree took {:?}",
            start.elapsed()
        );
        assert!(pages.is_empty(), "a cycle contains no real pages");
    }

    #[test]
    fn a_branching_page_tree_is_bounded_by_total_visits() {
        // Each node doubling into the next gives 2^depth visits without ever reaching a leaf —
        // the depth cap alone doesn't bound it.
        let depth = 24usize;
        let mut src = Vec::from(&b"%PDF-1.4\n"[..]);
        src.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        for k in 2..(2 + depth) {
            src.extend_from_slice(
                format!(
                    "{k} 0 obj\n<< /Type /Pages /Kids [{n} 0 R {n} 0 R] >>\nendobj\n",
                    n = k + 1
                )
                .as_bytes(),
            );
        }
        src.extend_from_slice(
            format!("{} 0 obj\n<< /Type /Page >>\nendobj\n", 2 + depth).as_bytes(),
        );
        let pdf = Pdf::parse(&src);
        let start = std::time::Instant::now();
        let _ = pdf.pages();
        assert!(
            start.elapsed() < std::time::Duration::from_secs(5),
            "branching page tree took {:?}",
            start.elapsed()
        );
    }

    #[test]
    fn an_object_stream_offset_past_usize_is_skipped_not_added() {
        // `1e30 as usize` saturates to `usize::MAX`; a plain `first + offset` then panics.
        let dict: Dict = vec![
            ("Type".into(), Obj::Name("ObjStm".into())),
            ("N".into(), Obj::Number(1.0)),
            ("First".into(), Obj::Number(8.0)),
        ];
        let mut objects = HashMap::new();
        objects.insert(
            1u32,
            Obj::Stream {
                dict,
                data: b"1 1e30 <</X 1>>".to_vec(),
            },
        );
        expand_object_streams(&mut objects);
        assert_eq!(objects.len(), 1, "the bad entry was skipped, not expanded");
    }

    #[test]
    fn repeated_content_references_are_bounded() {
        // `/Contents` naming one stream hundreds of times decodes it hundreds of times, so a
        // small file becomes gigabytes of work. Asserted against `page_content` directly, so the
        // test proves the cap without paying to extract text from 64 MB.
        use std::io::Write;
        let payload = b"BT (x) Tj ET ".repeat(200_000); // ~2.6 MB per decode
        let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(&payload).unwrap();
        let stream = enc.finish().unwrap();
        let mut src = Vec::from(&b"%PDF-1.4\n"[..]);
        src.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        src.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] >>\nendobj\n");
        // 400 × 2.6 MB is a gigabyte uncapped, from a file under 20 KB.
        let refs = "4 0 R ".repeat(400);
        src.extend_from_slice(
            format!("3 0 obj\n<< /Type /Page /Contents [{refs}] >>\nendobj\n").as_bytes(),
        );
        src.extend_from_slice(
            format!(
                "4 0 obj\n<< /Length {} /Filter /FlateDecode >>\nstream\n",
                stream.len()
            )
            .as_bytes(),
        );
        src.extend_from_slice(&stream);
        src.extend_from_slice(b"\nendstream\nendobj\n");
        let pdf = Pdf::parse(&src);
        let pages = pdf.pages();
        let content = pdf.page_content(pages[0]);
        assert!(
            content.len() <= MAX_PAGE_CONTENT + payload.len() + 1,
            "page content grew to {} bytes",
            content.len()
        );
    }

    #[test]
    fn garbage_content_terminates() {
        for junk in [&b"BT ((((("[..], b"TJ", b"", b"<<<<<<", b")))]]]"] {
            let _ = extract_text(junk, &HashMap::new());
        }
    }
}
