//! Parse push (`publishDiagnostics`) and pull (`textDocument/diagnostic`) diagnostic payloads.

use serde_json::Value;

use crate::{Diagnostic, DiagnosticsUpdate, PullReport, Severity};

/// Parse a `Diagnostic[]` into the parsed model + its raw JSON, kept in lockstep. A single
/// malformed entry is skipped rather than discarding the whole batch (which would also fail to
/// clear stale diagnostics) — one buggy or hostile diagnostic must not suppress the valid ones.
/// `raw` lets the client echo the overlapping diagnostics into a `codeAction` context (§6.1).
fn parse_diagnostic_array(arr: &[Value]) -> (Vec<Diagnostic>, Vec<Value>) {
    let mut diagnostics = Vec::new();
    let mut raw = Vec::new();
    for d in arr {
        if let Some(diag) = parse_one_diagnostic(d) {
            diagnostics.push(diag);
            raw.push(d.clone());
        }
    }
    (diagnostics, raw)
}

/// Parse a `publishDiagnostics` notification's params into our model.
pub(crate) fn parse_diagnostics(value: &Value) -> Option<DiagnosticsUpdate> {
    let params = value.get("params")?;
    let uri = params.get("uri")?.as_str()?.to_string();
    let arr = params
        .get("diagnostics")
        .and_then(|d| d.as_array())
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let (diagnostics, raw) = parse_diagnostic_array(arr);
    Some(DiagnosticsUpdate {
        uri,
        diagnostics,
        raw,
    })
}

/// Parse a `textDocument/diagnostic` (pull) result into a [`PullReport`] (§5.1). A `kind:
/// "unchanged"` report means "keep what you have"; a `full` report (the default for any other or
/// missing kind, including a `null` result → empty full report that clears) carries the fresh set.
/// `relatedDocuments` is ignored (we don't declare `relatedDocumentSupport`).
pub fn parse_diagnostic_report(result: &Value) -> PullReport {
    let result_id = result
        .get("resultId")
        .and_then(|v| v.as_str())
        .map(String::from);
    if result.get("kind").and_then(|k| k.as_str()) == Some("unchanged") {
        return PullReport::Unchanged { result_id };
    }
    let arr = result
        .get("items")
        .and_then(|d| d.as_array())
        .map(Vec::as_slice)
        .unwrap_or(&[]);
    let (diagnostics, raw) = parse_diagnostic_array(arr);
    PullReport::Full {
        result_id,
        diagnostics,
        raw,
    }
}

/// Parse a single diagnostic object, returning `None` (to be skipped) if it is malformed.
fn parse_one_diagnostic(d: &Value) -> Option<Diagnostic> {
    let range = d.get("range")?;
    let start = range.get("start")?;
    let end = range.get("end")?;
    let severity = match d.get("severity").and_then(|s| s.as_u64()) {
        Some(1) => Severity::Error,
        Some(2) => Severity::Warning,
        Some(3) => Severity::Info,
        _ => Severity::Hint,
    };
    // `code` is a string or a number (or a `{ value, target }` object in 3.16+).
    let code = match d.get("code") {
        Some(Value::String(s)) => Some(s.clone()),
        Some(Value::Number(n)) => Some(n.to_string()),
        Some(Value::Object(o)) => o.get("value").map(|v| match v {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        }),
        _ => None,
    };
    Some(Diagnostic {
        line: start.get("line")?.as_u64()? as u32,
        start_char16: start.get("character")?.as_u64()? as u32,
        end_line: end.get("line")?.as_u64()? as u32,
        end_char16: end.get("character")?.as_u64()? as u32,
        severity,
        message: d
            .get("message")
            .and_then(|m| m.as_str())
            .unwrap_or("")
            .to_string(),
        source: d.get("source").and_then(|s| s.as_str()).map(str::to_string),
        code,
    })
}
