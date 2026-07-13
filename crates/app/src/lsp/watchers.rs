//! File-watcher registrations for `workspace/didChangeWatchedFiles` (§8.1). The client owns file
//! watching (the editor already watches the project tree, §6); a server registers globs via
//! `client/registerCapability` and we forward matching disk changes back. This module is the pure
//! glob-parsing + matching core, unit-tested without a live connection.

use std::path::Path;

use globset::Glob;
use serde_json::Value;

/// LSP `FileChangeType` values (the notification's `type` field).
pub const CREATED: u8 = 1;
pub const CHANGED: u8 = 2;
pub const DELETED: u8 = 3;

/// One registered watcher: a compiled glob plus the change-kind bitmask it subscribed to
/// (`1 Create | 2 Change | 4 Delete`, default `7`).
pub struct FileWatcher {
    matcher: globset::GlobMatcher,
    kind: u8,
}

impl FileWatcher {
    /// Whether this watcher wants `path` for a `change_type` (one of [`CREATED`]/[`CHANGED`]/
    /// [`DELETED`]). The registration `kind` is a bitmask (1/2/4), so the 1/2/3 change *type* maps
    /// to its bit before the test.
    fn wants(&self, path: &Path, change_type: u8) -> bool {
        let bit = match change_type {
            CREATED => 1,
            CHANGED => 2,
            DELETED => 4,
            _ => 0,
        };
        self.kind & bit != 0 && self.matcher.is_match(path)
    }
}

/// Parse the `watchers` array of a `DidChangeWatchedFilesRegistrationOptions` into compiled
/// matchers. A `globPattern` is a plain glob string or a `RelativePattern { baseUri, pattern }`
/// (matched against absolute paths). Unparseable globs are skipped rather than failing the batch.
pub fn parse_watchers(register_options: &Value) -> Vec<FileWatcher> {
    let Some(arr) = register_options.get("watchers").and_then(|w| w.as_array()) else {
        return Vec::new();
    };
    arr.iter().filter_map(parse_one).collect()
}

fn parse_one(w: &Value) -> Option<FileWatcher> {
    let gp = w.get("globPattern")?;
    let pattern = match gp {
        // Plain glob, matched against absolute paths.
        Value::String(s) => normalize(s),
        // RelativePattern { baseUri, pattern } — anchor the glob under the base directory so an
        // absolute-path match works. `baseUri` is a `file://` string or a `{ uri }` object.
        Value::Object(_) => {
            let pat = gp.get("pattern")?.as_str()?;
            let base = gp
                .get("baseUri")
                .and_then(|b| b.as_str().or_else(|| b.get("uri").and_then(|u| u.as_str())))
                .and_then(|u| u.strip_prefix("file://"))
                .unwrap_or("");
            if base.is_empty() {
                normalize(pat)
            } else {
                format!("{}/{}", base.trim_end_matches('/'), pat)
            }
        }
        _ => return None,
    };
    let kind = w
        .get("kind")
        .and_then(|k| k.as_u64())
        .map(|k| k as u8)
        .unwrap_or(7);
    let matcher = Glob::new(&pattern).ok()?.compile_matcher();
    Some(FileWatcher { matcher, kind })
}

/// Anchor a bare, unrooted glob so it matches absolute paths. A pattern that is already absolute
/// (`/…`) or already starts with a `**/` recursive prefix is left as-is; anything else (e.g.
/// `*.rs`, `src/**/*.ts`, `Cargo.toml`) is prefixed with `**/` so it matches at any depth — the
/// relative-to-workspace-folder semantics the LSP intends.
fn normalize(pattern: &str) -> String {
    if pattern.starts_with('/') || pattern.starts_with("**/") {
        pattern.to_string()
    } else {
        format!("**/{pattern}")
    }
}

/// Whether any watcher in `watchers` subscribed to this `(path, change_type)` — the test for
/// whether a `didChangeWatchedFiles` notification should be sent for the change.
pub fn any_match<'a>(
    watchers: impl IntoIterator<Item = &'a FileWatcher>,
    path: &Path,
    change_type: u8,
) -> bool {
    watchers.into_iter().any(|w| w.wants(path, change_type))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::Path;

    fn watchers(opts: Value) -> Vec<FileWatcher> {
        parse_watchers(&opts)
    }
    fn matches(ws: &[FileWatcher], p: &str, ty: u8) -> bool {
        any_match(ws.iter(), Path::new(p), ty)
    }

    #[test]
    fn recursive_glob_matches_absolute_paths() {
        let ws = watchers(json!({ "watchers": [{ "globPattern": "**/*.rs" }] }));
        assert!(matches(&ws, "/home/u/proj/src/a.rs", CHANGED));
        assert!(matches(&ws, "/home/u/proj/a.rs", CHANGED));
        assert!(!matches(&ws, "/home/u/proj/a.toml", CHANGED));
    }

    #[test]
    fn bare_pattern_is_anchored_to_any_depth() {
        // `Cargo.toml` (no `**/`) must still match deep in the tree (relative-to-folder intent).
        let ws = watchers(json!({ "watchers": [{ "globPattern": "Cargo.toml" }] }));
        assert!(matches(&ws, "/w/proj/Cargo.toml", CHANGED));
        assert!(matches(&ws, "/w/proj/sub/Cargo.toml", CHANGED));
        assert!(!matches(&ws, "/w/proj/Cargo.lock", CHANGED));
    }

    #[test]
    fn brace_alternation_matches() {
        let ws = watchers(json!({ "watchers": [{ "globPattern": "**/*.{ts,tsx,js}" }] }));
        assert!(matches(&ws, "/p/a.ts", CHANGED));
        assert!(matches(&ws, "/p/a.tsx", CHANGED));
        assert!(!matches(&ws, "/p/a.rs", CHANGED));
    }

    #[test]
    fn kind_bitmask_filters_change_types() {
        // kind = 4 (Delete only): a Change is ignored, a Delete matches.
        let ws = watchers(json!({ "watchers": [{ "globPattern": "**/*.rs", "kind": 4 }] }));
        assert!(!matches(&ws, "/p/a.rs", CHANGED));
        assert!(matches(&ws, "/p/a.rs", DELETED));
        // Default kind (omitted) subscribes to all three.
        let all = watchers(json!({ "watchers": [{ "globPattern": "**/*.rs" }] }));
        assert!(matches(&all, "/p/a.rs", CREATED));
        assert!(matches(&all, "/p/a.rs", DELETED));
    }

    #[test]
    fn relative_pattern_anchors_to_base() {
        let ws = watchers(json!({
            "watchers": [{ "globPattern": { "baseUri": "file:///w/proj", "pattern": "**/*.rs" } }]
        }));
        assert!(matches(&ws, "/w/proj/src/a.rs", CHANGED));
        // Outside the base directory → no match.
        assert!(!matches(&ws, "/other/a.rs", CHANGED));
    }

    #[test]
    fn malformed_watcher_is_skipped() {
        // A bad glob is dropped; a valid sibling still compiles.
        let ws = watchers(json!({ "watchers": [
            { "globPattern": "[" },              // invalid glob
            { "globPattern": "**/*.rs" }
        ] }));
        assert_eq!(ws.len(), 1);
    }
}
