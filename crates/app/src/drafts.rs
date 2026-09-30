//! Crash-recovery drafts (industry-readiness §3): snapshot dirty buffer contents so a panic,
//! SIGKILL, or power loss does not lose unsaved work.
//!
//! Session restore only persists paths/cursors/scroll. Drafts are the companion that persist
//! **text**. They live under the per-user data dir, keyed by workspace root + file path (or an
//! untitled slot). Successful saves and intentional discards clear the matching draft.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// One recoverable buffer snapshot.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Draft {
    /// Absolute path when the buffer was path-backed; `None` for untitled.
    pub path: Option<PathBuf>,
    /// Stable slot for untitled drafts within a workspace (`untitled-0`, …).
    pub untitled_key: Option<String>,
    pub text: String,
    pub cursor: usize,
    pub scroll: usize,
    /// Encoding tag as a short string (`utf8`, `utf8bom`, `utf16le`, `utf16be`).
    pub encoding: String,
    /// Line ending tag (`lf`, `crlf`, `cr`).
    pub line_ending: String,
    pub mixed_line_endings: bool,
    pub lossy_decode: bool,
    pub revision: u64,
}

impl Draft {
    pub fn key(&self) -> String {
        if let Some(path) = &self.path {
            format!(
                "{:016x}",
                crate::files::fingerprint(path.to_string_lossy().as_bytes()).hash
            )
        } else {
            self.untitled_key
                .clone()
                .unwrap_or_else(|| "untitled-unknown".into())
        }
    }
}

/// Root of the drafts tree: `<data_dir>/drafts`. Overridable in tests.
pub fn default_drafts_root() -> Option<PathBuf> {
    directories::ProjectDirs::from("", "", "lumina").map(|d| d.data_dir().join("drafts"))
}

fn workspace_dir(drafts_root: &Path, workspace: &Path) -> PathBuf {
    let key = crate::files::fingerprint(workspace.to_string_lossy().as_bytes()).hash;
    drafts_root.join(format!("{key:016x}"))
}

fn draft_path(drafts_root: &Path, workspace: &Path, draft: &Draft) -> PathBuf {
    workspace_dir(drafts_root, workspace).join(format!("{}.toml", draft.key()))
}

/// Persist one draft atomically under `drafts_root` for `workspace`.
pub fn save_draft(drafts_root: &Path, workspace: &Path, draft: &Draft) -> std::io::Result<()> {
    let path = draft_path(drafts_root, workspace, draft);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let src = toml::to_string(draft).map_err(|e| std::io::Error::other(e.to_string()))?;
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, &src)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// Load every draft for `workspace` (best-effort; skips corrupt entries).
pub fn load_drafts(drafts_root: &Path, workspace: &Path) -> Vec<Draft> {
    let dir = workspace_dir(drafts_root, workspace);
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("toml") {
            continue;
        }
        let Ok(src) = std::fs::read_to_string(&path) else {
            continue;
        };
        if let Ok(draft) = toml::from_str::<Draft>(&src) {
            out.push(draft);
        }
    }
    out.sort_by_key(|a| a.key());
    out
}

/// Remove the draft that matches `path` (path-backed buffer).
pub fn clear_path_draft(drafts_root: &Path, workspace: &Path, path: &Path) {
    let draft = Draft {
        path: Some(path.to_path_buf()),
        untitled_key: None,
        text: String::new(),
        cursor: 0,
        scroll: 0,
        encoding: String::new(),
        line_ending: String::new(),
        mixed_line_endings: false,
        lossy_decode: false,
        revision: 0,
    };
    let _ = std::fs::remove_file(draft_path(drafts_root, workspace, &draft));
}

/// Remove an untitled draft by key.
#[allow(dead_code)] // exercised by unit tests; production clears via clear_workspace_drafts
pub fn clear_untitled_draft(drafts_root: &Path, workspace: &Path, key: &str) {
    let draft = Draft {
        path: None,
        untitled_key: Some(key.to_string()),
        text: String::new(),
        cursor: 0,
        scroll: 0,
        encoding: String::new(),
        line_ending: String::new(),
        mixed_line_endings: false,
        lossy_decode: false,
        revision: 0,
    };
    let _ = std::fs::remove_file(draft_path(drafts_root, workspace, &draft));
}

/// Wipe every draft for a workspace (intentional discard-all / clean quit after saves).
pub fn clear_workspace_drafts(drafts_root: &Path, workspace: &Path) {
    let dir = workspace_dir(drafts_root, workspace);
    let _ = std::fs::remove_dir_all(dir);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_root(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!(
            "lumina_drafts_{tag}_{}_{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    #[test]
    fn draft_roundtrips_and_clears() {
        let root = temp_root("rt");
        let ws = PathBuf::from("/proj/ws");
        let draft = Draft {
            path: Some(PathBuf::from("/proj/ws/a.rs")),
            untitled_key: None,
            text: "fn main() {}\n".into(),
            cursor: 3,
            scroll: 0,
            encoding: "utf8".into(),
            line_ending: "lf".into(),
            mixed_line_endings: false,
            lossy_decode: false,
            revision: 7,
        };
        save_draft(&root, &ws, &draft).unwrap();
        let loaded = load_drafts(&root, &ws);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].text, draft.text);
        assert_eq!(loaded[0].cursor, 3);
        clear_path_draft(&root, &ws, Path::new("/proj/ws/a.rs"));
        assert!(load_drafts(&root, &ws).is_empty());
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn untitled_drafts_use_stable_keys() {
        let root = temp_root("ut");
        let ws = PathBuf::from("/proj/ws");
        let draft = Draft {
            path: None,
            untitled_key: Some("untitled-0".into()),
            text: "scratch\n".into(),
            cursor: 0,
            scroll: 0,
            encoding: "utf8".into(),
            line_ending: "lf".into(),
            mixed_line_endings: false,
            lossy_decode: false,
            revision: 1,
        };
        save_draft(&root, &ws, &draft).unwrap();
        assert_eq!(load_drafts(&root, &ws).len(), 1);
        clear_untitled_draft(&root, &ws, "untitled-0");
        assert!(load_drafts(&root, &ws).is_empty());
        std::fs::remove_dir_all(&root).ok();
    }
}
