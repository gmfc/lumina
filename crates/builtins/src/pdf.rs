//! PDF viewer, implemented **as a plugin** (invariant #3).
//!
//! Claims `.pdf` and renders the document's *text* in a tab: a metadata header, then each page's
//! extracted content under a page rule. `lumina` itself learns nothing about PDF — this reaches
//! the editor through the same [`editor_plugin::ViewerSpec`] contribution any third-party viewer
//! would use, and disabling it in `[plugins]` hands `.pdf` back to the binary-file notice.
//!
//! ## Scope
//!
//! Text extraction, not rendering. It reads the text-showing operators of each page's content
//! stream and recovers reading order and line breaks; it does not lay out glyphs, so column
//! order in a multi-column document follows the content stream rather than the visual page.
//!
//! Not supported, and reported rather than silently wrong:
//! - **encrypted documents** — strings are ciphertext; extraction would emit noise;
//! - **scanned pages** — an image has no text operators, so such a document reports no text;
//! - image/legacy stream codecs (`DCTDecode`, `LZWDecode`, …), which carry no text anyway.
//!
//! When nothing can be extracted the tab shows a structural summary and points at the hex
//! viewer, so the user learns what the file *is* rather than staring at a blank pane.

use std::path::Path;

use editor_core::DocId;
use editor_plugin::{Contributions, Host, PanelLine, Plugin, Span, ViewerContent};

mod extract;
mod parse;

use extract::Pdf;

/// Largest PDF we parse. Parsing is a whole-file scan on the UI thread, so this is a latency
/// bound as much as a memory one; past it the notice points at the hex viewer instead.
const MAX_BYTES: u64 = 64 * 1024 * 1024;

/// Cap on rendered rows. A 5 000-page document would otherwise build a multi-million-row tab
/// that costs more to scroll than to read.
const MAX_LINES: usize = 200_000;

pub(crate) const VIEWER_ID: &str = "pdf.document";

pub(crate) struct PdfPlugin;

impl Plugin for PdfPlugin {
    fn id(&self) -> &str {
        "pdf"
    }

    fn contributions(&self) -> Contributions {
        Contributions::builder()
            .viewer(VIEWER_ID, "PDF Document", &["pdf"])
            .build()
    }

    fn render_viewer(&mut self, viewer_id: &str, doc: DocId, path: &Path, host: &mut dyn Host) {
        if viewer_id != VIEWER_ID {
            return;
        }
        host.set_viewer_content(doc, render(path));
    }
}

/// Read and render `path`, degrading to a stated reason at every step that can fail.
fn render(path: &Path) -> ViewerContent {
    let bytes = match readable_bytes(path) {
        Ok(bytes) => bytes,
        Err(reason) => return ViewerContent::status_only(reason),
    };
    let pdf = Pdf::parse(&bytes);
    if pdf.encrypted {
        return ViewerContent::status_only(
            "This PDF is encrypted — lumina can't extract its text. Use “View as Hex” to inspect \
             the raw bytes.",
        );
    }

    let pages = pdf.pages();
    let mut lines = info_lines(&pdf.info);
    let mut with_text = 0usize;
    for (i, page) in pages.iter().enumerate() {
        if lines.len() >= MAX_LINES {
            break;
        }
        lines.push(PanelLine::new(vec![Span::new(
            format!("── Page {} ──", i + 1),
            "dir",
        )]));
        if push_page_text(&mut lines, &pdf.page_text(page)) {
            with_text += 1;
        }
        lines.push(PanelLine::new(vec![Span::plain("")]));
    }

    ViewerContent {
        status: Some(summary(&pdf, pages.len(), with_text, lines.len())),
        lines,
    }
}

/// The file's bytes, or the sentence explaining why they can't be rendered.
fn readable_bytes(path: &Path) -> Result<Vec<u8>, String> {
    let len = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    if len > MAX_BYTES {
        return Err(format!(
            "This PDF is {} MB — too large to parse here. Use “View as Hex” to inspect it.",
            len / (1024 * 1024)
        ));
    }
    let bytes = std::fs::read(path).map_err(|e| format!("Could not read the file: {e}"))?;
    if !bytes.starts_with(b"%PDF-") {
        return Err("This file does not start with a %PDF- header — it may not be a PDF.".into());
    }
    Ok(bytes)
}

/// The `/Info` header block, with a blank line under it when there is anything to show.
fn info_lines(info: &[(String, String)]) -> Vec<PanelLine> {
    let mut lines: Vec<PanelLine> = info
        .iter()
        .map(|(key, value)| {
            PanelLine::new(vec![
                Span::new(format!("{key:<14}"), "dim"),
                Span::plain(value.clone()),
            ])
        })
        .collect();
    if !lines.is_empty() {
        lines.push(PanelLine::new(vec![Span::plain("")]));
    }
    lines
}

/// Append one page's text, collapsing runs of blank lines — the operator stream emits a break per
/// `ET`, which in a heavily-structured page means dozens of empty rows between paragraphs.
/// Returns whether the page had any text at all.
fn push_page_text(lines: &mut Vec<PanelLine>, text: &str) -> bool {
    let trimmed: Vec<&str> = text.lines().map(str::trim_end).collect();
    let has_text = trimmed.iter().any(|l| !l.trim().is_empty());
    let mut blank = false;
    for line in trimmed {
        let is_blank = line.trim().is_empty();
        if is_blank && blank {
            continue;
        }
        blank = is_blank;
        if lines.len() >= MAX_LINES {
            break;
        }
        lines.push(PanelLine::new(vec![Span::plain(line.to_string())]));
    }
    has_text
}

/// The status row: what was found, or why what was found is not what the user expected.
fn summary(pdf: &Pdf, pages: usize, with_text: usize, line_count: usize) -> String {
    if pages == 0 {
        return format!(
            "No pages found in {} objects — the file may be damaged. Use “View as Hex” to \
             inspect it.",
            pdf.object_count()
        );
    }
    if with_text == 0 {
        return format!(
            "{pages} page(s), none with extractable text — this is likely a scanned document. \
             Use “View as Hex” to inspect the raw bytes."
        );
    }
    let truncated = if line_count >= MAX_LINES {
        " (truncated)"
    } else {
        ""
    };
    format!("{pages} page(s), {with_text} with text{truncated}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Build a minimal but *valid* PDF around one page whose content stream is `content`,
    /// optionally Flate-compressed. Hand-written rather than generated so the fixtures stay
    /// hermetic and readable — and so a regression points at a specific byte.
    fn make_pdf(content: &[u8], compress: bool) -> Vec<u8> {
        let (stream, filter) = if compress {
            let mut enc =
                flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
            enc.write_all(content).unwrap();
            (enc.finish().unwrap(), "/Filter /FlateDecode ")
        } else {
            (content.to_vec(), "")
        };
        let mut out = Vec::from(&b"%PDF-1.4\n"[..]);
        out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        out.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
        out.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /Contents 4 0 R >>\nendobj\n",
        );
        out.extend_from_slice(
            format!("4 0 obj\n<< /Length {} {filter}>>\nstream\n", stream.len()).as_bytes(),
        );
        out.extend_from_slice(&stream);
        out.extend_from_slice(b"\nendstream\nendobj\n");
        out.extend_from_slice(b"trailer\n<< /Root 1 0 R /Info 5 0 R >>\n");
        out.extend_from_slice(
            b"5 0 obj\n<< /Title (Test Document) /Producer (lumina tests) >>\nendobj\n",
        );
        out.extend_from_slice(b"%%EOF\n");
        out
    }

    fn body(content: &ViewerContent) -> String {
        content
            .lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.text.as_str()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn render_bytes(bytes: &[u8]) -> ViewerContent {
        let path = std::env::temp_dir().join(format!(
            "lumina_pdf_{}_{:p}.pdf",
            std::process::id(),
            bytes.as_ptr()
        ));
        std::fs::write(&path, bytes).unwrap();
        let content = render(&path);
        std::fs::remove_file(&path).ok();
        content
    }

    const CONTENT: &[u8] = b"BT /F1 12 Tf 72 720 Td (Hello, PDF) Tj 0 -14 Td (Second line) Tj ET";

    #[test]
    fn extracts_text_from_an_uncompressed_pdf() {
        let content = render_bytes(&make_pdf(CONTENT, false));
        let text = body(&content);
        assert!(text.contains("Hello, PDF"), "{text}");
        assert!(text.contains("Second line"), "{text}");
        assert!(text.contains("Page 1"), "page rule is drawn: {text}");
        assert_eq!(
            content.status.as_deref(),
            Some("1 page(s), 1 with text"),
            "status reports the page count"
        );
    }

    #[test]
    fn a_flate_compressed_stream_extracts_identically() {
        // The overwhelmingly common real-world shape; if only the uncompressed path worked, the
        // viewer would look fine in tests and be blank on every actual document.
        let plain = body(&render_bytes(&make_pdf(CONTENT, false)));
        let deflated = body(&render_bytes(&make_pdf(CONTENT, true)));
        assert_eq!(plain, deflated);
        assert!(deflated.contains("Hello, PDF"));
    }

    #[test]
    fn document_metadata_is_shown() {
        let text = body(&render_bytes(&make_pdf(CONTENT, false)));
        assert!(text.contains("Test Document"), "title: {text}");
        assert!(text.contains("lumina tests"), "producer: {text}");
    }

    #[test]
    fn an_encrypted_document_says_so_instead_of_showing_noise() {
        let mut pdf = make_pdf(CONTENT, false);
        pdf.extend_from_slice(b"trailer\n<< /Encrypt 9 0 R >>\n");
        let content = render_bytes(&pdf);
        assert!(
            content
                .status
                .as_deref()
                .is_some_and(|s| s.contains("encrypted")),
            "{:?}",
            content.status
        );
        assert!(content.lines.is_empty(), "no garbage body");
    }

    #[test]
    fn a_page_with_no_text_reports_a_scan_rather_than_an_empty_pane() {
        let content = render_bytes(&make_pdf(b"q 1 0 0 1 0 0 cm /Im0 Do Q", false));
        assert!(
            content
                .status
                .as_deref()
                .is_some_and(|s| s.contains("no extractable text") || s.contains("none with")),
            "{:?}",
            content.status
        );
    }

    #[test]
    fn pages_inside_an_object_stream_are_found() {
        // PDF 1.5+ writers put page dictionaries in an ObjStm; a scanner that ignored those
        // would report "no pages" on most modern documents.
        let inner = b"<< /Type /Catalog /Pages 2 0 R >> << /Type /Pages /Kids [3 0 R] /Count 1 >> << /Type /Page /Parent 2 0 R /Contents 4 0 R >>";
        // Offsets of the three objects within `inner`.
        let first_len = inner
            .iter()
            .position(|&b| b == b'>')
            .map(|_| 0)
            .unwrap_or(0);
        let _ = first_len;
        let o1 = 0usize;
        let o2 = extract_offset(inner, 1);
        let o3 = extract_offset(inner, 2);
        let header = format!("1 {o1} 2 {o2} 3 {o3} ");
        let mut payload = header.clone().into_bytes();
        payload.extend_from_slice(inner);

        let mut enc = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(&payload).unwrap();
        let stream = enc.finish().unwrap();

        let mut out = Vec::from(&b"%PDF-1.5\n"[..]);
        out.extend_from_slice(
            format!(
                "6 0 obj\n<< /Type /ObjStm /N 3 /First {} /Length {} /Filter /FlateDecode >>\nstream\n",
                header.len(),
                stream.len()
            )
            .as_bytes(),
        );
        out.extend_from_slice(&stream);
        out.extend_from_slice(b"\nendstream\nendobj\n");
        out.extend_from_slice(
            format!("4 0 obj\n<< /Length {} >>\nstream\n", CONTENT.len()).as_bytes(),
        );
        out.extend_from_slice(CONTENT);
        out.extend_from_slice(b"\nendstream\nendobj\n%%EOF\n");

        let text = body(&render_bytes(&out));
        assert!(text.contains("Hello, PDF"), "{text}");
    }

    /// Byte offset of the `n`-th `<<`-delimited object inside a concatenated payload.
    fn extract_offset(buf: &[u8], n: usize) -> usize {
        buf.windows(2)
            .enumerate()
            .filter(|(_, w)| *w == b"<<")
            .map(|(i, _)| i)
            .nth(n)
            .unwrap_or(0)
    }

    #[test]
    fn a_non_pdf_file_is_reported_not_parsed() {
        let content = render_bytes(b"not a pdf at all\n");
        assert!(
            content
                .status
                .as_deref()
                .is_some_and(|s| s.contains("%PDF-")),
            "{:?}",
            content.status
        );
    }

    #[test]
    fn truncated_and_garbage_files_return_instead_of_panicking() {
        let full = make_pdf(CONTENT, true);
        // Every prefix of a real PDF, plus pure noise: none may panic or hang.
        for cut in [8, 40, 100, full.len() / 2, full.len() - 1] {
            let _ = render_bytes(&full[..cut.min(full.len())]);
        }
        let mut noise = Vec::from(&b"%PDF-1.7\n"[..]);
        noise.extend((0u8..=255).cycle().take(4096));
        let _ = render_bytes(&noise);
    }

    #[test]
    fn a_missing_file_reports_rather_than_panicking() {
        let content = render(std::path::Path::new("/definitely/not/here.pdf"));
        assert!(content.status.is_some());
        assert!(content.lines.is_empty());
    }
}
