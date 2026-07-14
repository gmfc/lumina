//! Pure `editor-lsp` → kernel-primitive translators (no `App`, no I/O).

/// Translate an `editor-lsp` diagnostic into the kernel's primitive `LspDiagnostic` (so the
/// `diagnostics` plugin owns the model without depending on `editor-lsp`).
pub(super) fn to_primitive_diag(d: editor_lsp::Diagnostic) -> editor_plugin::LspDiagnostic {
    use editor_lsp::Severity as S;
    use editor_plugin::LspSeverity as P;
    editor_plugin::LspDiagnostic {
        line: d.line,
        start_char16: d.start_char16,
        end_line: d.end_line,
        end_char16: d.end_char16,
        severity: match d.severity {
            S::Error => P::Error,
            S::Warning => P::Warning,
            S::Info => P::Info,
            S::Hint => P::Hint,
        },
        message: d.message,
        source: d.source,
        code: d.code,
    }
}

/// Translate an `editor-lsp` semantic token into the kernel's primitive `LspSemanticToken`.
pub(super) fn to_primitive_semantic_token(
    t: editor_lsp::SemanticToken,
) -> editor_plugin::LspSemanticToken {
    editor_plugin::LspSemanticToken {
        line: t.line,
        start_char16: t.start_char16,
        length: t.length,
        token_type: t.token_type,
        modifiers: t.modifiers,
    }
}

/// Translate a resolved `editor-lsp` code lens into the kernel's primitive `LspCodeLens` (drops the
/// raw JSON; the title is guaranteed present by the manager).
pub(super) fn to_primitive_code_lens(l: editor_lsp::CodeLens) -> editor_plugin::LspCodeLens {
    editor_plugin::LspCodeLens {
        line: l.line,
        char16: l.char16,
        title: l.title.unwrap_or_default(),
    }
}

/// Translate an `editor-lsp` inlay hint into the kernel's primitive `LspInlayHint`.
pub(super) fn to_primitive_inlay_hint(h: editor_lsp::InlayHint) -> editor_plugin::LspInlayHint {
    editor_plugin::LspInlayHint {
        line: h.line,
        char16: h.char16,
        label: h.label,
        kind: h.kind,
        pad_left: h.pad_left,
        pad_right: h.pad_right,
    }
}

/// Translate an `editor-lsp` completion item into the kernel's primitive `LspCompletionItem`.
pub(super) fn to_primitive_completion(
    it: editor_lsp::CompletionItem,
) -> editor_plugin::LspCompletionItem {
    editor_plugin::LspCompletionItem {
        label: it.label,
        detail: it.detail,
        insert_text: it.insert_text,
        kind: it.kind,
        additional_edits: it
            .additional_edits
            .into_iter()
            .map(to_primitive_text_edit)
            .collect(),
        is_snippet: it.is_snippet,
        data: it.data,
        command: it.command.map(|c| (c.command, c.arguments)),
    }
}

/// Translate an `editor-lsp` text edit into the kernel's primitive `LspTextEdit` (same coordinates).
/// Whether a `WorkspaceEdit` entry is stale and must be dropped (§2.4): it declares a version and
/// the buffer's last-synced version has moved past it. No declared version, or an unknown current
/// version, means don't reject (best-effort — the legacy `changes` map is unversioned).
pub(super) fn edit_is_stale(edit_version: Option<i64>, current: Option<i64>) -> bool {
    matches!((edit_version, current), (Some(v), Some(c)) if v != c)
}

pub(super) fn to_primitive_text_edit(te: editor_lsp::TextEdit) -> editor_plugin::LspTextEdit {
    editor_plugin::LspTextEdit {
        start_line: te.start_line,
        start_char16: te.start_char16,
        end_line: te.end_line,
        end_char16: te.end_char16,
        new_text: te.new_text,
    }
}

/// Resolve an `editor-lsp` location's URI to a filesystem path and package it as the primitive
/// [`editor_plugin::LspLocation`] the `lsp-nav` plugin jumps to. `None` for a non-`file:` URI.
pub(super) fn to_primitive_location(
    loc: &editor_lsp::Location,
) -> Option<editor_plugin::LspLocation> {
    let path = crate::lsp::path_from_uri(&loc.uri)?;
    Some(editor_plugin::LspLocation {
        path: path.to_string_lossy().into_owned(),
        line: loc.line,
        character: loc.character,
    })
}

/// A `file:line:col` label for a location picker row (plan §2.3).
pub(super) fn location_label(loc: &editor_lsp::Location) -> String {
    let file = crate::lsp::path_from_uri(&loc.uri)
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| loc.uri.clone());
    format!("{file}:{}:{}", loc.line + 1, loc.character + 1)
}

#[cfg(test)]
mod tests {
    use super::edit_is_stale;

    #[test]
    fn edit_staleness_matrix() {
        assert!(edit_is_stale(Some(5), Some(7))); // buffer moved past the edit's version → drop
        assert!(!edit_is_stale(Some(5), Some(5))); // versions match → apply
        assert!(!edit_is_stale(None, Some(7))); // unversioned (legacy changes map) → apply
        assert!(!edit_is_stale(Some(5), None)); // current version unknown → best-effort apply
    }
}
