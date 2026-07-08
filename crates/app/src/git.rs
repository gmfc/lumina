//! Read-only git diff for the change gutter (plan §4.1). Computes a per-line
//! added/modified/deleted map for a file against `HEAD`.
//!
//! SPEC-NOTE: the spec suggests `git2` (libgit2) or shelling out. We shell out to `git` — no
//! heavy native dependency to build in CI, and lumina already relies on `git` for ignore
//! rules. The compute runs off the main thread (see `worker::spawn_git`); this module is the
//! pure "diff text → line map" core plus the process invocation, kept separate so the parser
//! is unit-testable without a repo.

use std::collections::HashMap;
use std::path::Path;

/// Change status of a single line in the working tree relative to `HEAD`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineStatus {
    Added,
    Modified,
    /// Lines were removed at this point; the marker sits on the surviving line above the gap.
    Deleted,
}

/// 0-based document line → its change status. Absent lines are unchanged.
pub type LineStatuses = HashMap<usize, LineStatus>;

/// Compute the change map for `path` against `HEAD`, running `git` with `root` as the repo
/// working directory. Returns an empty map when the file is untracked, outside a repo, or git
/// is unavailable — the gutter simply shows nothing.
pub fn compute(root: &Path, path: &Path) -> LineStatuses {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "--no-optional-locks",
            "diff",
            "--no-color",
            "-U0",
            "HEAD",
            "--",
        ])
        .arg(path)
        .output();
    match output {
        Ok(o) if o.status.success() => parse_diff(&String::from_utf8_lossy(&o.stdout)),
        _ => LineStatuses::new(),
    }
}

/// Parse a `-U0` unified diff into a per-line status map, reading only the `@@` hunk headers.
/// A hunk that only adds lines is `Added`; one that only removes is `Deleted`; one that does
/// both is `Modified`.
pub fn parse_diff(diff: &str) -> LineStatuses {
    let mut map = LineStatuses::new();
    for line in diff.lines() {
        let Some(rest) = line.strip_prefix("@@ ") else {
            continue;
        };
        let Some(((_, old_count), (new_start, new_count))) = parse_hunk_header(rest) else {
            continue;
        };
        if new_count == 0 {
            // Pure deletion: mark the surviving line just above the removed gap.
            map.insert(new_start.saturating_sub(1), LineStatus::Deleted);
        } else {
            let status = if old_count == 0 {
                LineStatus::Added
            } else {
                LineStatus::Modified
            };
            for l in new_start..new_start + new_count {
                map.insert(l.saturating_sub(1), status);
            }
        }
    }
    map
}

/// Parse the two ranges from a hunk header body like `-a,b +c,d @@ context`.
fn parse_hunk_header(s: &str) -> Option<((usize, usize), (usize, usize))> {
    let mut parts = s.split_whitespace();
    let old = parse_range(parts.next()?.strip_prefix('-')?)?;
    let new = parse_range(parts.next()?.strip_prefix('+')?)?;
    Some((old, new))
}

/// Parse `start,count` (or bare `start`, count defaulting to 1) into `(start, count)`.
fn parse_range(s: &str) -> Option<(usize, usize)> {
    let mut it = s.split(',');
    let start: usize = it.next()?.parse().ok()?;
    let count: usize = match it.next() {
        Some(c) => c.parse().ok()?,
        None => 1,
    };
    Some((start, count))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_added_modified_deleted() {
        // Line 2 modified (1 old, 1 new); lines 5-6 added (0 old); a deletion after line 9.
        let diff = "\
diff --git a/f b/f
index 000..111 100644
--- a/f
+++ b/f
@@ -2 +2 @@
-old
+new
@@ -4,0 +5,2 @@
+added one
+added two
@@ -9,2 +9,0 @@
-gone one
-gone two
";
        let m = parse_diff(diff);
        assert_eq!(m.get(&1), Some(&LineStatus::Modified)); // new line 2 → idx 1
        assert_eq!(m.get(&4), Some(&LineStatus::Added)); // new line 5 → idx 4
        assert_eq!(m.get(&5), Some(&LineStatus::Added)); // new line 6 → idx 5
        assert_eq!(m.get(&8), Some(&LineStatus::Deleted)); // deletion above line 9 → idx 8
    }

    #[test]
    fn empty_diff_is_empty_map() {
        assert!(parse_diff("").is_empty());
        assert!(parse_diff("diff --git a/f b/f\nno hunks here\n").is_empty());
    }

    #[test]
    fn compute_on_a_temp_repo() {
        // A real repo: commit a file, edit it, expect the edited line flagged. Skips silently
        // if `git` isn't available (keeps CI green on minimal images).
        let Ok(dir) = tempdir() else {
            return;
        };
        let run = |args: &[&str]| {
            std::process::Command::new("git")
                .arg("-C")
                .arg(&dir)
                .args(args)
                .output()
        };
        if run(&["init", "-q"]).is_err() {
            return;
        }
        let _ = run(&["config", "user.email", "t@t"]);
        let _ = run(&["config", "user.name", "t"]);
        let file = dir.join("a.txt");
        std::fs::write(&file, "one\ntwo\nthree\n").unwrap();
        let _ = run(&["add", "a.txt"]);
        if run(&["commit", "-qm", "init"])
            .map(|o| !o.status.success())
            .unwrap_or(true)
        {
            return; // commit failed (e.g. no git identity) — don't fail the suite
        }
        std::fs::write(&file, "one\nCHANGED\nthree\n").unwrap();
        let m = compute(&dir, &file);
        assert_eq!(m.get(&1), Some(&LineStatus::Modified));
        std::fs::remove_dir_all(&dir).ok();
    }

    fn tempdir() -> std::io::Result<std::path::PathBuf> {
        // A unique scratch dir without pulling in a temp-dir crate.
        let base = std::env::temp_dir().join(format!(
            "lumina_git_test_{}_{}",
            std::process::id(),
            COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&base)?;
        Ok(base)
    }

    static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
}
