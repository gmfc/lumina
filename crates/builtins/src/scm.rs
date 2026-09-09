//! Source control — the changed-files view, staging and committing, implemented **as a plugin**
//! (invariant #3).
//!
//! The editor already had a per-line change gutter (`crates/app/src/git.rs`), but nothing at the
//! *repository* level: no list of what changed, no branch, no way to stage or commit. The gutter
//! also throws the diff body away — it keeps only `@@` headers — so it could never have grown
//! into this.
//!
//! Every `git` invocation runs on a worker thread through [`Host::spawn_job`], because a status
//! or a commit on a large repository takes long enough to drop frames. Results come back as
//! [`Event::JobComplete`] carrying raw stdout, tagged with a generation so a stale refresh is
//! dropped. The plugin reaches the editor only through `Host`: the file list is a contributed
//! `Sidebar` panel, the commit message box is the generic `Prompt` port, and opening a file goes
//! through [`Host::open_path`].

use std::path::{Path, PathBuf};
use std::process::Command;

use editor_plugin::{
    Contributions, Event, Host, Key, KeyCode, PanelContent, PanelLine, PanelLocation, Plugin,
    Prompt, PromptField, PromptPlacement, Span,
};

/// Cap on listed entries. A first commit in a large tree reports every file; past this the list
/// stops being useful and starts being a wall.
const ENTRY_CAP: usize = 500;

/// One changed path, as reported by `git status --porcelain=v1`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ScmEntry {
    /// The staged (index) status letter: `M`, `A`, `D`, `R`, `?`, or a space for "unchanged".
    pub(crate) index: char,
    /// The unstaged (worktree) status letter.
    pub(crate) worktree: char,
    pub(crate) path: PathBuf,
}

impl ScmEntry {
    /// Whether anything about this path is already staged.
    fn is_staged(&self) -> bool {
        self.index != ' ' && self.index != '?'
    }

    /// A short human label for the two status letters, e.g. `"modified"`, `"staged + modified"`.
    fn label(&self) -> &'static str {
        match (self.index, self.worktree) {
            ('?', _) => "untracked",
            (_, 'M') if self.is_staged() => "staged + modified",
            ('M', _) => "staged",
            ('A', _) => "added",
            ('D', _) | (_, 'D') => "deleted",
            ('R', _) => "renamed",
            (_, 'M') => "modified",
            _ => "changed",
        }
    }

    /// The style key for the row, so severity reads at a glance.
    fn style(&self) -> &'static str {
        match (self.index, self.worktree) {
            ('?', _) => "dim",
            _ if self.is_staged() => "match",
            _ => "text",
        }
    }
}

/// Parse `git status --porcelain=v1 -b -z` output.
///
/// `-z` rather than the line-oriented form on purpose: without it git quotes and escapes any path
/// containing a space, a quote or a non-ASCII byte, and every consumer has to re-implement that
/// unquoting. With `-z` records are NUL-terminated and paths are literal.
///
/// Returns `(branch, entries)`. A rename entry is followed by an extra record holding the origin
/// path, which is consumed and discarded — the destination is what the user acts on.
pub(crate) fn parse_status(out: &[u8]) -> (Option<String>, Vec<ScmEntry>) {
    let mut branch = None;
    let mut entries = Vec::new();
    let mut records = out.split(|b| *b == 0).filter(|r| !r.is_empty());
    let mut pending_rename = false;
    for rec in records.by_ref() {
        if pending_rename {
            pending_rename = false; // this record is the rename's origin path
            continue;
        }
        let text = String::from_utf8_lossy(rec);
        if let Some(rest) = text.strip_prefix("## ") {
            // `## main...origin/main [ahead 1]` — the local name is up to the first `...` or space.
            let name = rest
                .split("...")
                .next()
                .unwrap_or(rest)
                .split_whitespace()
                .next()
                .unwrap_or(rest);
            branch = Some(name.to_string());
            continue;
        }
        let mut chars = text.chars();
        let (Some(index), Some(worktree)) = (chars.next(), chars.next()) else {
            continue;
        };
        // Skip the single separating space; the rest is the path verbatim.
        let path = text.get(3..).unwrap_or_default();
        if path.is_empty() {
            continue;
        }
        if index == 'R' || worktree == 'R' {
            pending_rename = true;
        }
        entries.push(ScmEntry {
            index,
            worktree,
            path: PathBuf::from(path),
        });
    }
    (branch, entries)
}

/// Run a `git` subcommand in `root`, returning raw stdout (empty on any failure). Blocking —
/// call on a worker thread.
fn git(root: &Path, args: &[&str]) -> Vec<u8> {
    Command::new("git")
        .arg("-C")
        .arg(root)
        .arg("--no-optional-locks")
        .args(args)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| o.stdout)
        .unwrap_or_default()
}

/// The source-control feature as a plugin.
#[derive(Default)]
pub(crate) struct ScmPlugin {
    branch: Option<String>,
    entries: Vec<ScmEntry>,
    selected: usize,
    open: bool,
    /// Monotonic run id, so a status that lands after a newer one is dropped.
    generation: u64,
    /// Set while a commit message is being typed.
    commit_message: Option<String>,
}

impl ScmPlugin {
    const ID: &'static str = "scm";
    const PANEL: &'static str = "scm.changes";
    const PROMPT: &'static str = "scm.commit";

    /// Kick off a `git status` on a worker thread.
    fn refresh(&mut self, host: &mut dyn Host) {
        self.generation += 1;
        let root = host.root().to_path_buf();
        let work = Box::new(move || git(&root, &["status", "--porcelain=v1", "-b", "-z"]));
        host.spawn_job(format!("scm-status:{}", self.generation), work);
    }

    /// Run a mutating git command, then refresh. The refresh is what redraws the panel, so the
    /// list can never drift from the repository.
    fn run_git(&mut self, host: &mut dyn Host, args: Vec<String>) {
        self.generation += 1;
        let root = host.root().to_path_buf();
        let work = Box::new(move || {
            let borrowed: Vec<&str> = args.iter().map(String::as_str).collect();
            git(&root, &borrowed);
            // Re-read the status in the same job, so the panel updates in one round trip rather
            // than needing a second event to notice the change landed.
            git(&root, &["status", "--porcelain=v1", "-b", "-z"])
        });
        host.spawn_job(format!("scm-status:{}", self.generation), work);
    }

    fn selected_entry(&self) -> Option<&ScmEntry> {
        self.entries.get(self.selected)
    }

    fn stage_selected(&mut self, host: &mut dyn Host) {
        let Some(entry) = self.selected_entry().cloned() else {
            return;
        };
        let path = entry.path.to_string_lossy().into_owned();
        // `add -A` so a deletion is staged as a deletion rather than skipped.
        self.run_git(host, vec!["add".into(), "-A".into(), "--".into(), path]);
    }

    fn unstage_selected(&mut self, host: &mut dyn Host) {
        let Some(entry) = self.selected_entry().cloned() else {
            return;
        };
        let path = entry.path.to_string_lossy().into_owned();
        self.run_git(
            host,
            vec!["restore".into(), "--staged".into(), "--".into(), path],
        );
    }

    fn begin_commit(&mut self, host: &mut dyn Host) {
        if !self.entries.iter().any(ScmEntry::is_staged) {
            host.notify("Nothing staged to commit".into());
            return;
        }
        self.commit_message = Some(String::new());
        self.publish_commit_prompt(host, None);
    }

    fn publish_commit_prompt(&self, host: &mut dyn Host, error: Option<String>) {
        let Some(message) = &self.commit_message else {
            return;
        };
        let staged = self.entries.iter().filter(|e| e.is_staged()).count();
        let mut prompt = Prompt::new(Self::ID, Self::PROMPT, PromptPlacement::Center);
        prompt.title = Some(format!("Commit {staged} staged file(s)"));
        prompt.fields = vec![PromptField::new("Message", message.clone())];
        prompt.footer = Some("[Enter] Commit   [Esc] Cancel".into());
        prompt.error = error;
        host.set_prompt(prompt);
    }

    fn commit(&mut self, host: &mut dyn Host) {
        let Some(message) = self.commit_message.clone() else {
            return;
        };
        if message.trim().is_empty() {
            self.publish_commit_prompt(host, Some("A commit needs a message".into()));
            return;
        }
        self.commit_message = None;
        host.dismiss_prompt();
        self.run_git(host, vec!["commit".into(), "-m".into(), message]);
        host.notify("Committed".into());
    }

    fn move_selection(&mut self, delta: isize) {
        if self.entries.is_empty() {
            return;
        }
        let n = self.entries.len() as isize;
        self.selected = (self.selected as isize + delta).rem_euclid(n) as usize;
    }

    /// Publish the panel, or clear it when closed.
    fn render(&self, host: &mut dyn Host) {
        if !self.open {
            host.set_panel(Self::PANEL, PanelContent::default());
            return;
        }
        let head = match &self.branch {
            Some(b) => format!("on {b}"),
            // No branch means git said nothing useful: not a repo, no git on PATH, or a fresh
            // repo with no HEAD yet. Say so rather than showing a convincing empty list.
            None => "not a git repository".to_string(),
        };
        let staged = self.entries.iter().filter(|e| e.is_staged()).count();
        let mut lines = vec![PanelLine::new(vec![Span::new(
            format!("{head}  ({} changed, {staged} staged)", self.entries.len()),
            "title",
        )])];
        let mut selected_line = 0;
        for (i, entry) in self.entries.iter().take(ENTRY_CAP).enumerate() {
            if i == self.selected {
                selected_line = lines.len();
            }
            lines.push(
                PanelLine::new(vec![
                    Span::new(format!("{}{} ", entry.index, entry.worktree), "dim"),
                    Span::new(entry.path.to_string_lossy().into_owned(), entry.style()),
                    Span::new(format!("  {}", entry.label()), "dim"),
                ])
                .payload(entry.path.to_string_lossy().into_owned()),
            );
        }
        if self.entries.len() > ENTRY_CAP {
            lines.push(PanelLine::new(vec![Span::new(
                format!("  … and {} more", self.entries.len() - ENTRY_CAP),
                "dim",
            )]));
        }
        if self.entries.is_empty() && self.branch.is_some() {
            lines.push(PanelLine::new(vec![Span::new(
                "Nothing to commit, working tree clean",
                "dim",
            )]));
        }
        host.set_panel(
            Self::PANEL,
            PanelContent {
                lines,
                selected: selected_line,
            },
        );
    }
}

impl Plugin for ScmPlugin {
    fn id(&self) -> &str {
        Self::ID
    }

    fn contributions(&self) -> Contributions {
        Contributions::builder()
            .panel(Self::PANEL, "Source Control", PanelLocation::Sidebar)
            .command("scm.show", "Source Control: Show Changes")
            .command("scm.refresh", "Source Control: Refresh")
            .command("scm.stage", "Source Control: Stage File")
            .command("scm.unstage", "Source Control: Unstage File")
            .command("scm.commit", "Source Control: Commit Staged")
            .command("scm.next", "Source Control: Select Next")
            .command("scm.prev", "Source Control: Select Previous")
            .build()
    }

    fn run_command(&mut self, command_id: &str, host: &mut dyn Host) -> bool {
        match command_id {
            "scm.show" => {
                self.open = true;
                self.refresh(host);
            }
            "scm.refresh" => self.refresh(host),
            "scm.stage" => self.stage_selected(host),
            "scm.unstage" => self.unstage_selected(host),
            "scm.commit" => self.begin_commit(host),
            "scm.next" => self.move_selection(1),
            "scm.prev" => self.move_selection(-1),
            _ => return false,
        }
        self.render(host);
        true
    }

    fn on_prompt_key(&mut self, prompt_id: &str, key: Key, host: &mut dyn Host) -> bool {
        if prompt_id != Self::PROMPT || self.commit_message.is_none() {
            return false;
        }
        match key.code {
            KeyCode::Esc => {
                self.commit_message = None;
                host.dismiss_prompt();
            }
            KeyCode::Enter => self.commit(host),
            KeyCode::Backspace => {
                if let Some(m) = self.commit_message.as_mut() {
                    m.pop();
                }
                self.publish_commit_prompt(host, None);
            }
            KeyCode::Char(c) if !key.ctrl && !key.alt => {
                if let Some(m) = self.commit_message.as_mut() {
                    m.push(c);
                }
                self.publish_commit_prompt(host, None);
            }
            _ => {}
        }
        true
    }

    fn on_panel_activate(&mut self, panel_id: &str, payload: &str, host: &mut dyn Host) {
        if panel_id != Self::PANEL {
            return;
        }
        // Paths are repo-relative; resolve against the root before opening.
        let path = host.root().join(payload);
        host.open_path(&path);
        if let Some(i) = self
            .entries
            .iter()
            .position(|e| e.path.to_string_lossy() == payload)
        {
            self.selected = i;
        }
        self.render(host);
    }

    fn on_event(&mut self, event: &Event, host: &mut dyn Host) {
        match event {
            Event::JobComplete { id, payload } => {
                let Some(gen) = id
                    .strip_prefix("scm-status:")
                    .and_then(|g| g.parse::<u64>().ok())
                else {
                    return;
                };
                if gen != self.generation {
                    return; // a newer refresh is already in flight
                }
                let (branch, entries) = parse_status(payload);
                self.branch = branch;
                self.entries = entries;
                if self.selected >= self.entries.len() {
                    self.selected = self.entries.len().saturating_sub(1);
                }
                self.render(host);
            }
            // A save changes the working tree, so the list is stale the moment it happens.
            Event::DidSave(_) if self.open => self.refresh(host),
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_branch_and_entries() {
        // `-z` records are NUL-terminated, so paths are literal — no quoting to undo.
        let out = b"## main...origin/main [ahead 1]\0 M src/a.rs\0?? new.txt\0M  staged.rs\0";
        let (branch, entries) = parse_status(out);
        assert_eq!(branch.as_deref(), Some("main"));
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].path, PathBuf::from("src/a.rs"));
        assert_eq!((entries[0].index, entries[0].worktree), (' ', 'M'));
        assert!(!entries[0].is_staged());
        assert_eq!(entries[1].label(), "untracked");
        assert!(entries[2].is_staged());
    }

    #[test]
    fn a_path_with_a_space_survives() {
        let out = b"## main\0 M my file.txt\0";
        let (_, entries) = parse_status(out);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].path, PathBuf::from("my file.txt"));
    }

    /// A rename reports the destination and then the origin as a separate record. The origin must
    /// not be mistaken for another changed file.
    #[test]
    fn a_rename_consumes_its_origin_record() {
        let out = b"## main\0R  new/name.rs\0old/name.rs\0 M other.rs\0";
        let (_, entries) = parse_status(out);
        assert_eq!(entries.len(), 2, "the origin path is not a second entry");
        assert_eq!(entries[0].path, PathBuf::from("new/name.rs"));
        assert_eq!(entries[1].path, PathBuf::from("other.rs"));
    }

    #[test]
    fn empty_output_is_no_branch_and_no_entries() {
        let (branch, entries) = parse_status(b"");
        assert!(
            branch.is_none(),
            "no branch means 'not a repo', not 'clean'"
        );
        assert!(entries.is_empty());
    }

    #[test]
    fn a_detached_head_still_reports_something() {
        let out = b"## HEAD (no branch)\0";
        let (branch, _) = parse_status(out);
        assert_eq!(branch.as_deref(), Some("HEAD"));
    }
}
