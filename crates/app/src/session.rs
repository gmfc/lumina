//! Session restore (plan §6): persist the open files + per-file cursor/scroll on exit,
//! keyed by project root, and restore them on the next launch in that directory.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionEntry {
    pub path: PathBuf,
    pub cursor: usize,
    pub scroll: usize,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Session {
    pub files: Vec<SessionEntry>,
    pub active: usize,
}

/// Path of the session file for a given project root (namespaced by a hash of the root).
fn session_path(root: &Path) -> Option<PathBuf> {
    let dirs = directories::ProjectDirs::from("", "", "lumina")?;
    let key = crate::files::fingerprint(root.to_string_lossy().as_bytes()).hash;
    Some(
        dirs.data_dir()
            .join("sessions")
            .join(format!("{key:016x}.toml")),
    )
}

/// Save a session for `root`.
pub fn save(root: &Path, session: &Session) {
    let Some(path) = session_path(root) else {
        return;
    };
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(src) = toml::to_string(session) {
        // Write atomically (temp + rename) like file saves do: a crash mid-write would
        // otherwise leave a truncated TOML that `load` silently discards.
        let tmp = path.with_extension("toml.tmp");
        if std::fs::write(&tmp, &src).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }
}

/// Load a session for `root`, if one exists.
pub fn load(root: &Path) -> Option<Session> {
    let path = session_path(root)?;
    let src = std::fs::read_to_string(path).ok()?;
    toml::from_str(&src).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_through_toml() {
        let s = Session {
            files: vec![SessionEntry {
                path: PathBuf::from("/x/a.rs"),
                cursor: 42,
                scroll: 3,
            }],
            active: 0,
        };
        let src = toml::to_string(&s).unwrap();
        let back: Session = toml::from_str(&src).unwrap();
        assert_eq!(back.files.len(), 1);
        assert_eq!(back.files[0].cursor, 42);
    }
}
