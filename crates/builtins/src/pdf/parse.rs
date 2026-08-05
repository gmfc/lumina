//! A small, total PDF object reader: the object model, a lexer, and the stream filters.
//!
//! "Total" is the design constraint. This parses bytes an editor user picked at random, on the
//! UI thread, in a process that must not die — so every read is bounds-checked, every loop is
//! bounded, and malformed input yields a *shorter* result rather than a panic. There is no
//! `unwrap` on parsed data anywhere in this module, and the workspace forbids `unsafe`.
//!
//! Deliberately partial in scope: enough of PDF to pull text out of a document, not a
//! conforming implementation. What's missing is listed in [`super`]'s docs.

use std::io::Read;

/// A PDF object. Dictionaries keep insertion order in a `Vec` rather than hashing: a PDF
/// dictionary has a handful of keys, and linear lookup on that beats a `HashMap`'s allocation.
#[derive(Debug, Clone, PartialEq)]
pub(super) enum Obj {
    Null,
    Bool(bool),
    Number(f64),
    /// A literal `(…)` or hex `<…>` string, as raw bytes — the text decoder maps them to
    /// characters later, using the font's encoding.
    Str(Vec<u8>),
    Name(String),
    Array(Vec<Obj>),
    Dict(Dict),
    /// A stream: its dictionary plus the *still-encoded* bytes. Decoding is deferred because
    /// most streams in a document (images, fonts) are never looked at.
    Stream {
        dict: Dict,
        data: Vec<u8>,
    },
    /// An indirect reference `N G R`.
    Ref(u32),
}

/// A PDF dictionary: ordered key/value pairs.
pub(super) type Dict = Vec<(String, Obj)>;

impl Obj {
    pub(super) fn as_f64(&self) -> Option<f64> {
        match self {
            Obj::Number(n) => Some(*n),
            _ => None,
        }
    }

    pub(super) fn as_name(&self) -> Option<&str> {
        match self {
            Obj::Name(n) => Some(n),
            _ => None,
        }
    }

    pub(super) fn as_array(&self) -> Option<&[Obj]> {
        match self {
            Obj::Array(a) => Some(a),
            _ => None,
        }
    }

    /// The object's dictionary, whether it is a bare dict or a stream.
    pub(super) fn dict(&self) -> Option<&Dict> {
        match self {
            Obj::Dict(d) | Obj::Stream { dict: d, .. } => Some(d),
            _ => None,
        }
    }

    pub(super) fn get(&self, key: &str) -> Option<&Obj> {
        self.dict()?.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }
}

/// Look up `key` in a dictionary.
pub(super) fn get<'a>(dict: &'a Dict, key: &str) -> Option<&'a Obj> {
    dict.iter().find(|(k, _)| k == key).map(|(_, v)| v)
}

/// PDF whitespace (ISO 32000-1 table 1).
fn is_ws(b: u8) -> bool {
    matches!(b, b'\0' | b'\t' | b'\n' | 0x0c | b'\r' | b' ')
}

/// PDF delimiters (ISO 32000-1 table 2).
fn is_delim(b: u8) -> bool {
    matches!(
        b,
        b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
    )
}

fn is_regular(b: u8) -> bool {
    !is_ws(b) && !is_delim(b)
}

/// A cursor over PDF syntax. Every method is total: at end of input it returns `None` rather
/// than indexing past the buffer.
pub(super) struct Lexer<'a> {
    pub(super) buf: &'a [u8],
    pub(super) pos: usize,
}

/// Nesting cap for arrays/dictionaries. A hand-crafted file can nest `[[[[…` arbitrarily deep;
/// this turns what would be a stack overflow into a truncated parse.
const MAX_DEPTH: usize = 64;

impl<'a> Lexer<'a> {
    pub(super) fn new(buf: &'a [u8]) -> Lexer<'a> {
        Lexer { buf, pos: 0 }
    }

    pub(super) fn at(buf: &'a [u8], pos: usize) -> Lexer<'a> {
        Lexer {
            buf,
            pos: pos.min(buf.len()),
        }
    }

    fn peek(&self) -> Option<u8> {
        self.buf.get(self.pos).copied()
    }

    /// Skip whitespace and `%` comments.
    pub(super) fn skip_ws(&mut self) {
        while let Some(b) = self.peek() {
            if is_ws(b) {
                self.pos += 1;
            } else if b == b'%' {
                while let Some(c) = self.peek() {
                    self.pos += 1;
                    if c == b'\n' || c == b'\r' {
                        break;
                    }
                }
            } else {
                break;
            }
        }
    }

    /// Read the next bare keyword (an operator, `obj`, `stream`, `true`, …), if one is here.
    pub(super) fn keyword(&mut self) -> Option<&'a [u8]> {
        self.skip_ws();
        let start = self.pos;
        while self.peek().is_some_and(is_regular) {
            self.pos += 1;
        }
        (self.pos > start).then(|| &self.buf[start..self.pos])
    }

    /// Parse one object. Returns `None` at end of input or on a delimiter that can't start one
    /// (`]`, `>>`), which is how array/dict parsing detects its terminator.
    pub(super) fn object(&mut self) -> Option<Obj> {
        self.object_at_depth(0)
    }

    /// Step over input that isn't an object, guaranteeing forward progress.
    ///
    /// Consumes a **whole** keyword rather than one byte. That distinction is the difference
    /// between linear and quadratic: a damaged file whose array holds a megabyte-long run of
    /// letters would otherwise re-scan that run once per byte and hang the editor, since
    /// [`Self::number_or_keyword`] rewinds after reading a keyword that isn't a number.
    fn skip_unparseable(&mut self) {
        if self.keyword().is_none() {
            self.pos += 1; // a delimiter — a keyword scan makes no progress on one
        }
    }

    fn object_at_depth(&mut self, depth: usize) -> Option<Obj> {
        if depth > MAX_DEPTH {
            return None;
        }
        self.skip_ws();
        match self.peek()? {
            b'/' => Some(Obj::Name(self.name()?)),
            b'(' => Some(Obj::Str(self.literal_string())),
            b'[' => {
                self.pos += 1;
                let mut items = Vec::new();
                loop {
                    self.skip_ws();
                    match self.peek() {
                        None => break,
                        Some(b']') => {
                            self.pos += 1;
                            break;
                        }
                        _ => match self.object_at_depth(depth + 1) {
                            Some(o) => items.push(o),
                            None => self.skip_unparseable(),
                        },
                    }
                }
                Some(Obj::Array(items))
            }
            b'<' => {
                if self.buf.get(self.pos + 1) == Some(&b'<') {
                    self.pos += 2;
                    let dict = self.dict_body(depth)?;
                    Some(self.maybe_stream(dict))
                } else {
                    Some(Obj::Str(self.hex_string()))
                }
            }
            b'>' | b']' | b')' | b'}' | b'{' => None,
            _ => self.number_or_keyword(),
        }
    }

    /// Key/value pairs up to `>>` (the `<<` is already consumed).
    fn dict_body(&mut self, depth: usize) -> Option<Dict> {
        let mut dict = Dict::new();
        loop {
            self.skip_ws();
            match self.peek() {
                None => break,
                Some(b'>') => {
                    // `>>`, or a stray `>` we step over rather than spin on.
                    self.pos += if self.buf.get(self.pos + 1) == Some(&b'>') {
                        2
                    } else {
                        1
                    };
                    break;
                }
                Some(b'/') => {
                    let key = self.name()?;
                    let value = self.object_at_depth(depth + 1).unwrap_or(Obj::Null);
                    dict.push((key, value));
                }
                // A malformed dict (a value where a key belongs): drop the value and continue,
                // so one bad entry doesn't cost us the whole dictionary.
                _ => {
                    if self.object_at_depth(depth + 1).is_none() {
                        self.skip_unparseable();
                    }
                }
            }
        }
        Some(dict)
    }

    /// After a dictionary, consume a `stream … endstream` body if one follows.
    fn maybe_stream(&mut self, dict: Dict) -> Obj {
        let save = self.pos;
        self.skip_ws();
        if !self.buf[self.pos..].starts_with(b"stream") {
            self.pos = save;
            return Obj::Dict(dict);
        }
        self.pos += b"stream".len();
        // The keyword is followed by CRLF or LF (never a bare CR, per the spec — but tolerate it).
        if self.buf.get(self.pos) == Some(&b'\r') {
            self.pos += 1;
        }
        if self.buf.get(self.pos) == Some(&b'\n') {
            self.pos += 1;
        }
        let start = self.pos;

        // Prefer the declared `/Length`, but only when it actually lands on `endstream`: an
        // indirect or stale Length is common in damaged files, and trusting it blindly yields
        // truncated or over-long data. Otherwise fall back to searching for the keyword.
        let declared = get(&dict, "Length")
            .and_then(Obj::as_f64)
            .filter(|n| *n >= 0.0)
            .map(|n| n as usize)
            .filter(|n| start + n <= self.buf.len())
            .filter(|n| {
                let mut probe = Lexer::at(self.buf, start + n);
                probe.skip_ws();
                self.buf[probe.pos..].starts_with(b"endstream")
            });
        let end = match declared {
            Some(len) => start + len,
            None => match find(&self.buf[start..], b"endstream") {
                Some(rel) => {
                    // Back off the EOL the writer inserted before `endstream`.
                    let mut e = start + rel;
                    if e > start && self.buf[e - 1] == b'\n' {
                        e -= 1;
                    }
                    if e > start && self.buf[e - 1] == b'\r' {
                        e -= 1;
                    }
                    e
                }
                None => self.buf.len(),
            },
        };
        let data = self.buf[start..end.max(start)].to_vec();
        self.pos = end;
        // Step past `endstream` so the caller resumes after the body.
        if let Some(rel) = find(&self.buf[self.pos..], b"endstream") {
            self.pos += rel + b"endstream".len();
        } else {
            self.pos = self.buf.len();
        }
        Obj::Stream { dict, data }
    }

    /// `/Name`, with `#xx` hex escapes.
    fn name(&mut self) -> Option<String> {
        if self.peek()? != b'/' {
            return None;
        }
        self.pos += 1;
        let mut out = String::new();
        while let Some(b) = self.peek() {
            if !is_regular(b) {
                break;
            }
            self.pos += 1;
            if b == b'#' {
                let hi = self.peek().and_then(hex_val);
                let lo = self.buf.get(self.pos + 1).copied().and_then(hex_val);
                if let (Some(hi), Some(lo)) = (hi, lo) {
                    self.pos += 2;
                    out.push((hi * 16 + lo) as char);
                    continue;
                }
            }
            out.push(b as char);
        }
        Some(out)
    }

    /// `(literal string)` with nested parens and backslash escapes.
    fn literal_string(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        self.pos += 1; // '('
        let mut depth = 1usize;
        while let Some(b) = self.peek() {
            self.pos += 1;
            match b {
                b'\\' => {
                    let Some(e) = self.peek() else { break };
                    self.pos += 1;
                    match e {
                        b'n' => out.push(b'\n'),
                        b'r' => out.push(b'\r'),
                        b't' => out.push(b'\t'),
                        b'b' => out.push(0x08),
                        b'f' => out.push(0x0c),
                        b'\n' => {} // line continuation
                        b'\r' => {
                            if self.peek() == Some(b'\n') {
                                self.pos += 1;
                            }
                        }
                        b'0'..=b'7' => {
                            // Up to three octal digits.
                            let mut v = (e - b'0') as u16;
                            for _ in 0..2 {
                                match self.peek() {
                                    Some(d @ b'0'..=b'7') => {
                                        v = v * 8 + (d - b'0') as u16;
                                        self.pos += 1;
                                    }
                                    _ => break,
                                }
                            }
                            out.push(v as u8);
                        }
                        other => out.push(other),
                    }
                }
                b'(' => {
                    depth += 1;
                    out.push(b);
                }
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        break;
                    }
                    out.push(b);
                }
                other => out.push(other),
            }
        }
        out
    }

    /// `<48656c6c6f>`; an odd final digit is padded with 0, per the spec.
    fn hex_string(&mut self) -> Vec<u8> {
        self.pos += 1; // '<'
        let mut out = Vec::new();
        let mut hi: Option<u8> = None;
        while let Some(b) = self.peek() {
            self.pos += 1;
            if b == b'>' {
                break;
            }
            let Some(v) = hex_val(b) else { continue };
            match hi.take() {
                None => hi = Some(v),
                Some(h) => out.push(h * 16 + v),
            }
        }
        if let Some(h) = hi {
            out.push(h * 16);
        }
        out
    }

    /// A number, a `N G R` reference, or one of the bare keywords that are objects.
    fn number_or_keyword(&mut self) -> Option<Obj> {
        let save = self.pos;
        let word = self.keyword()?;
        match word {
            b"true" => return Some(Obj::Bool(true)),
            b"false" => return Some(Obj::Bool(false)),
            b"null" => return Some(Obj::Null),
            _ => {}
        }
        // Not a number — an operator, or `obj`/`endobj`. Rewind so the caller can read it as a
        // keyword instead; the content-stream tokenizer depends on this.
        let Some(num) = std::str::from_utf8(word)
            .ok()
            .and_then(|t| t.parse::<f64>().ok())
        else {
            self.pos = save;
            return None;
        };

        // `N G R` needs two tokens of lookahead; on anything else, rewind to just past the
        // number so the caller sees exactly what it would have without the attempt.
        if num >= 0.0 && num.fract() == 0.0 {
            let after_num = self.pos;
            if let Some(gen) = self.keyword().and_then(|w| std::str::from_utf8(w).ok()) {
                if gen.parse::<u32>().is_ok() && self.keyword() == Some(b"R".as_ref()) {
                    return Some(Obj::Ref(num as u32));
                }
            }
            self.pos = after_num;
        }
        Some(Obj::Number(num))
    }
}

fn hex_val(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// First index of `needle` in `haystack`.
pub(super) fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

// --- stream filters ---------------------------------------------------------------------

/// Largest decoded stream we keep. A "zip bomb" PDF can declare a tiny Flate stream that
/// inflates to gigabytes; the decoder is capped so a hostile file costs bounded memory.
const MAX_DECODED: usize = 64 * 1024 * 1024;

/// Decode a stream's bytes through its `/Filter` chain. Returns `None` for a filter we don't
/// implement (image codecs, LZW) — the caller then skips that stream rather than showing noise.
pub(super) fn decode_stream(dict: &Dict, data: &[u8]) -> Option<Vec<u8>> {
    let filters: Vec<String> = match get(dict, "Filter") {
        None => Vec::new(),
        Some(Obj::Name(n)) => vec![n.clone()],
        Some(Obj::Array(a)) => a
            .iter()
            .filter_map(|o| o.as_name().map(String::from))
            .collect(),
        Some(_) => return None,
    };
    let mut out = data.to_vec();
    for filter in &filters {
        out = match filter.as_str() {
            "FlateDecode" | "Fl" => inflate(&out)?,
            "ASCIIHexDecode" | "AHx" => ascii_hex_decode(&out),
            "ASCII85Decode" | "A85" => ascii85_decode(&out),
            "RunLengthDecode" | "RL" => run_length_decode(&out),
            // LZWDecode, DCTDecode, JPXDecode, CCITTFaxDecode, JBIG2Decode: image/legacy
            // codecs with no text to contribute.
            _ => return None,
        };
    }
    Some(apply_predictor(dict, out))
}

/// zlib-wrapped DEFLATE, falling back to raw DEFLATE — real files carry both, and some are
/// written with a corrupt first byte that only the raw decoder survives.
fn inflate(data: &[u8]) -> Option<Vec<u8>> {
    fn read_capped<R: Read>(mut r: R) -> Option<Vec<u8>> {
        let mut out = Vec::new();
        // A partial inflate is still useful (a truncated PDF's first pages read fine), so a
        // read error keeps whatever was decoded rather than discarding it.
        let _ = (&mut r).take(MAX_DECODED as u64).read_to_end(&mut out);
        (!out.is_empty()).then_some(out)
    }
    read_capped(flate2::read::ZlibDecoder::new(data))
        .or_else(|| read_capped(flate2::read::DeflateDecoder::new(data)))
}

fn ascii_hex_decode(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut hi: Option<u8> = None;
    for &b in data {
        if b == b'>' {
            break;
        }
        let Some(v) = hex_val(b) else { continue };
        match hi.take() {
            None => hi = Some(v),
            Some(h) => out.push(h * 16 + v),
        }
    }
    if let Some(h) = hi {
        out.push(h * 16);
    }
    out
}

fn ascii85_decode(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut group = [0u8; 5];
    let mut n = 0usize;
    let mut i = 0usize;
    // Skip a leading `<~` if present.
    if data.starts_with(b"<~") {
        i = 2;
    }
    while i < data.len() {
        let b = data[i];
        i += 1;
        if b == b'~' {
            break;
        }
        if is_ws(b) {
            continue;
        }
        if b == b'z' && n == 0 {
            out.extend_from_slice(&[0, 0, 0, 0]);
            continue;
        }
        if !(b'!'..=b'u').contains(&b) {
            continue;
        }
        group[n] = b - b'!';
        n += 1;
        if n == 5 {
            push_base85(&mut out, &group, 5);
            n = 0;
        }
    }
    if n > 1 {
        for slot in group.iter_mut().skip(n) {
            *slot = 84; // pad with 'u'
        }
        push_base85(&mut out, &group, n);
    }
    out
}

/// Expand one base-85 group, keeping `n - 1` of the four decoded bytes (a short final group
/// encodes fewer bytes).
fn push_base85(out: &mut Vec<u8>, group: &[u8; 5], n: usize) {
    let mut value: u32 = 0;
    for &g in group {
        value = value.wrapping_mul(85).wrapping_add(g as u32);
    }
    let bytes = value.to_be_bytes();
    out.extend_from_slice(&bytes[..n.saturating_sub(1).min(4)]);
}

fn run_length_decode(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0usize;
    while i < data.len() {
        let len = data[i];
        i += 1;
        match len {
            128 => break,
            0..=127 => {
                let n = len as usize + 1;
                let end = (i + n).min(data.len());
                out.extend_from_slice(&data[i..end]);
                i = end;
            }
            _ => {
                let Some(&b) = data.get(i) else { break };
                i += 1;
                out.extend(std::iter::repeat_n(b, 257 - len as usize));
            }
        }
    }
    out
}

/// Undo a PNG predictor (`/DecodeParms /Predictor >= 10`). Object streams and cross-reference
/// streams routinely use predictor 12, and skipping this step turns them into noise.
fn apply_predictor(dict: &Dict, data: Vec<u8>) -> Vec<u8> {
    let Some(parms) = get(dict, "DecodeParms").and_then(Obj::dict) else {
        return data;
    };
    let predictor = get(parms, "Predictor").and_then(Obj::as_f64).unwrap_or(1.0) as i64;
    if predictor < 10 {
        return data; // 1 = none; 2 (TIFF) is not used by the streams we read
    }
    let colors = get(parms, "Colors").and_then(Obj::as_f64).unwrap_or(1.0) as usize;
    let bpc = get(parms, "BitsPerComponent")
        .and_then(Obj::as_f64)
        .unwrap_or(8.0) as usize;
    let columns = get(parms, "Columns").and_then(Obj::as_f64).unwrap_or(1.0) as usize;
    let bpp = (colors * bpc).div_ceil(8).max(1);
    let row_len = (columns * colors * bpc).div_ceil(8);
    if row_len == 0 {
        return data;
    }

    let mut out: Vec<u8> = Vec::with_capacity(data.len());
    let mut prev = vec![0u8; row_len];
    let mut row = vec![0u8; row_len];
    let mut i = 0usize;
    while i < data.len() {
        let tag = data[i];
        i += 1;
        let end = (i + row_len).min(data.len());
        let n = end - i;
        row[..n].copy_from_slice(&data[i..end]);
        row[n..].fill(0);
        i = end;
        for x in 0..row_len {
            let a = if x >= bpp { row[x - bpp] } else { 0 };
            let b = prev[x];
            let c = if x >= bpp { prev[x - bpp] } else { 0 };
            row[x] = match tag {
                0 => row[x],
                1 => row[x].wrapping_add(a),
                2 => row[x].wrapping_add(b),
                3 => row[x].wrapping_add(((a as u16 + b as u16) / 2) as u8),
                4 => row[x].wrapping_add(paeth(a, b, c)),
                _ => row[x],
            };
        }
        out.extend_from_slice(&row);
        prev.copy_from_slice(&row);
        if n < row_len {
            break;
        }
    }
    out
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = a as i16 + b as i16 - c as i16;
    let (pa, pb, pc) = (
        (p - a as i16).abs(),
        (p - b as i16).abs(),
        (p - c as i16).abs(),
    );
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(src: &[u8]) -> Option<Obj> {
        Lexer::new(src).object()
    }

    #[test]
    fn parses_scalars() {
        assert_eq!(parse(b"42"), Some(Obj::Number(42.0)));
        assert_eq!(parse(b"-1.5"), Some(Obj::Number(-1.5)));
        assert_eq!(parse(b"true"), Some(Obj::Bool(true)));
        assert_eq!(parse(b"null"), Some(Obj::Null));
        assert_eq!(parse(b"/Type"), Some(Obj::Name("Type".into())));
        // `#xx` escapes in names.
        assert_eq!(parse(b"/A#20B"), Some(Obj::Name("A B".into())));
    }

    #[test]
    fn parses_literal_string_escapes() {
        assert_eq!(parse(b"(hi)"), Some(Obj::Str(b"hi".to_vec())));
        assert_eq!(parse(br"(a\nb)"), Some(Obj::Str(b"a\nb".to_vec())));
        assert_eq!(parse(br"(a\(b\))"), Some(Obj::Str(b"a(b)".to_vec())));
        // Nested parens need no escaping.
        assert_eq!(parse(b"(a(b)c)"), Some(Obj::Str(b"a(b)c".to_vec())));
        // Octal escapes.
        assert_eq!(parse(br"(\101)"), Some(Obj::Str(b"A".to_vec())));
        // A backslash-newline is a line continuation, not content.
        assert_eq!(parse(b"(a\\\nb)"), Some(Obj::Str(b"ab".to_vec())));
    }

    #[test]
    fn parses_hex_string_with_odd_padding() {
        assert_eq!(parse(b"<48656C6C6F>"), Some(Obj::Str(b"Hello".to_vec())));
        // A trailing odd digit is padded with 0 (spec §7.3.4.3).
        assert_eq!(parse(b"<4>"), Some(Obj::Str(vec![0x40])));
    }

    #[test]
    fn parses_arrays_dicts_and_refs() {
        assert_eq!(
            parse(b"[1 2 /Three]"),
            Some(Obj::Array(vec![
                Obj::Number(1.0),
                Obj::Number(2.0),
                Obj::Name("Three".into())
            ]))
        );
        let d = parse(b"<< /Type /Page /Parent 3 0 R /Count 2 >>").unwrap();
        assert_eq!(d.get("Type").and_then(Obj::as_name), Some("Page"));
        assert_eq!(d.get("Parent"), Some(&Obj::Ref(3)));
        assert_eq!(d.get("Count").and_then(Obj::as_f64), Some(2.0));
    }

    #[test]
    fn a_number_followed_by_a_non_reference_is_not_swallowed() {
        // `1 2` must parse as two numbers — the `N G R` lookahead has to rewind cleanly.
        let mut lex = Lexer::new(b"1 2 /X");
        assert_eq!(lex.object(), Some(Obj::Number(1.0)));
        assert_eq!(lex.object(), Some(Obj::Number(2.0)));
        assert_eq!(lex.object(), Some(Obj::Name("X".into())));
    }

    #[test]
    fn reads_a_stream_by_declared_length() {
        let src = b"<< /Length 5 >>\nstream\nHELLO\nendstream";
        match parse(src) {
            Some(Obj::Stream { data, .. }) => assert_eq!(data, b"HELLO"),
            other => panic!("expected a stream, got {other:?}"),
        }
    }

    #[test]
    fn a_wrong_length_falls_back_to_searching_for_endstream() {
        // Damaged files declare stale lengths constantly; trusting one blindly truncates text.
        let src = b"<< /Length 999 >>\nstream\nHELLO\nendstream";
        match parse(src) {
            Some(Obj::Stream { data, .. }) => assert_eq!(data, b"HELLO"),
            other => panic!("expected a stream, got {other:?}"),
        }
    }

    #[test]
    fn comments_are_skipped() {
        assert_eq!(parse(b"% a comment\n/Name"), Some(Obj::Name("Name".into())));
    }

    #[test]
    fn malformed_input_terminates_without_panicking() {
        // Each of these used to be a plausible way to hang or index out of bounds.
        for src in [
            &b"<<"[..],
            b"[",
            b"(unterminated",
            b"<abc",
            b"<< /K",
            b"<< /Length 5 >>\nstream\nab",
            b"[[[[[[[[[[[[[[[[[[[[",
            b")",
            b"",
        ] {
            let _ = parse(src);
        }
    }

    #[test]
    fn a_long_unparseable_run_inside_an_array_is_linear_not_quadratic() {
        // `[` + a megabyte of letters + `]`: the array's error path must consume the whole
        // keyword, not retry one byte at a time (which is O(n²) and hangs the editor).
        let mut src = vec![b'['];
        src.extend(std::iter::repeat_n(b'a', 1_000_000));
        src.push(b']');
        let start = std::time::Instant::now();
        let _ = Lexer::new(&src).object();
        assert!(
            start.elapsed() < std::time::Duration::from_secs(2),
            "array error recovery went quadratic: {:?}",
            start.elapsed()
        );
    }

    #[test]
    fn deep_nesting_is_bounded() {
        let src: Vec<u8> = std::iter::repeat_n(b'[', MAX_DEPTH * 4).collect();
        let _ = parse(&src); // must return, not overflow the stack
    }

    #[test]
    fn inflates_zlib_and_raw_deflate() {
        use flate2::write::{DeflateEncoder, ZlibEncoder};
        use std::io::Write;

        let plain = b"the quick brown fox".repeat(10);
        let mut z = ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        z.write_all(&plain).unwrap();
        let zlib = z.finish().unwrap();
        assert_eq!(inflate(&zlib).as_deref(), Some(plain.as_slice()));

        let mut d = DeflateEncoder::new(Vec::new(), flate2::Compression::default());
        d.write_all(&plain).unwrap();
        let raw = d.finish().unwrap();
        assert_eq!(inflate(&raw).as_deref(), Some(plain.as_slice()));

        assert_eq!(inflate(b"not compressed at all"), None);
    }

    #[test]
    fn decodes_ascii_filters() {
        let dict: Dict = vec![("Filter".into(), Obj::Name("ASCIIHexDecode".into()))];
        assert_eq!(
            decode_stream(&dict, b"48656c6c6f>").as_deref(),
            Some(&b"Hello"[..])
        );

        let dict: Dict = vec![("Filter".into(), Obj::Name("ASCII85Decode".into()))];
        // "Hello" in base85 (from the canonical encoder), terminated by `~>`.
        assert_eq!(
            decode_stream(&dict, b"87cURDZ~>").as_deref(),
            Some(&b"Hello"[..])
        );
    }

    #[test]
    fn run_length_round_trips_a_literal_and_a_repeat() {
        // 2 → copy 3 literal bytes; 254 → repeat the next byte 3 times; 128 → EOD.
        let encoded = [2u8, b'a', b'b', b'c', 254, b'z', 128];
        assert_eq!(run_length_decode(&encoded), b"abczzz");
    }

    #[test]
    fn an_unknown_filter_is_declined_rather_than_shown_as_noise() {
        let dict: Dict = vec![("Filter".into(), Obj::Name("DCTDecode".into()))];
        assert_eq!(decode_stream(&dict, b"\xff\xd8\xff"), None);
    }

    #[test]
    fn png_up_predictor_is_undone() {
        // Two rows of 3 bytes with predictor 12 (PNG Up): the second row's deltas resolve
        // against the first.
        let dict: Dict = vec![(
            "DecodeParms".into(),
            Obj::Dict(vec![
                ("Predictor".into(), Obj::Number(12.0)),
                ("Columns".into(), Obj::Number(3.0)),
            ]),
        )];
        let raw = vec![2u8, 1, 2, 3, 2, 1, 1, 1];
        assert_eq!(apply_predictor(&dict, raw), vec![1, 2, 3, 2, 3, 4]);
    }

    #[test]
    fn find_locates_and_reports_absence() {
        assert_eq!(find(b"abcdef", b"cd"), Some(2));
        assert_eq!(find(b"abcdef", b"xy"), None);
        assert_eq!(find(b"ab", b"abcdef"), None);
        assert_eq!(find(b"abc", b""), None);
    }
}
