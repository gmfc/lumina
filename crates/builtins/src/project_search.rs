//! Project-wide search, implemented **as a plugin** (invariant #3).
//!
//! The ripgrep engine (`grep-searcher` + `grep-regex` + `ignore`) runs off the main thread via
//! the background-job port ([`Host::spawn_job`]); results come back as [`Event::JobComplete`],
//! tagged with a generation so a stale search is dropped. The plugin owns the whole search model
//! and reaches the editor only through `Host`: the query box is the generic [`Prompt`] port (a
//! `Panel`-placement prompt used for key routing only), results are published as a `PanelContent`
//! the app draws in the bottom dock, and a hit opens through [`Host::open_path_at`].

use std::path::{Path, PathBuf};

use editor_core::{Change, Transaction};
use editor_plugin::{
    Contributions, Event, Host, Key, KeyCode, PanelContent, PanelLine, PanelLocation, Plugin,
    Prompt, PromptPlacement, Span,
};
use grep_regex::RegexMatcherBuilder;
use grep_searcher::sinks::UTF8;
use grep_searcher::Searcher;
use ignore::WalkBuilder;
use regex::{Regex, RegexBuilder};

/// Total hits kept, so a broad query on a big tree stays bounded.
const HIT_CAP: usize = 2000;

/// One match: a file, a 1-based line number, and the matching line's text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SearchHit {
    pub(crate) path: PathBuf,
    pub(crate) line: usize,
    pub(crate) text: String,
}

/// Run a project search under `root` for `pattern`. Blocking — call on a worker thread.
pub(crate) fn run_search(
    root: &Path,
    pattern: &str,
    case_sensitive: bool,
    cap: usize,
) -> Vec<SearchHit> {
    let matcher = match RegexMatcherBuilder::new()
        .case_insensitive(!case_sensitive)
        .build(pattern)
    {
        Ok(m) => m,
        Err(_) => return Vec::new(),
    };
    let mut hits: Vec<SearchHit> = Vec::new();
    let walker = WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .filter_entry(|e| e.file_name() != ".git")
        .build();
    for entry in walker.flatten() {
        if hits.len() >= cap {
            break;
        }
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let path = entry.path().to_path_buf();
        let mut searcher = Searcher::new();
        let path_for_sink = path.clone();
        // best-effort: an unreadable, binary, or non-UTF-8 file is an expected skip, not an
        // error — drop the per-file io::Result (no log dep exists here) and move on.
        let _ = searcher.search_path(
            &matcher,
            &path,
            UTF8(|line_num, line| {
                hits.push(SearchHit {
                    path: path_for_sink.clone(),
                    line: line_num as usize,
                    text: line.trim_end().to_string(),
                });
                Ok(hits.len() < cap)
            }),
        );
    }
    hits
}

/// Rewrite `pattern` -> `replacement` in every file in `paths`, on disk. Blocking — call on a
/// worker thread.
///
/// Only for files the editor does not have open: an open buffer is the truth for its path, so
/// writing underneath it would either be clobbered by the next save or resurface as an
/// external-change conflict. Those go through a `Transaction` instead (invariant #1), which is
/// also what makes the change undoable.
///
/// A file that cannot be read as UTF-8 is skipped rather than mangled — the same rule the open
/// path follows. Returns the number of files actually rewritten.
pub(crate) fn replace_on_disk(paths: &[PathBuf], re: &Regex, replacement: &str) -> usize {
    let mut changed = 0;
    for path in paths {
        let Ok(before) = std::fs::read(path) else {
            continue;
        };
        let Ok(before) = String::from_utf8(before) else {
            continue; // not text we can round-trip; leave it exactly as it is
        };
        let after = re.replace_all(&before, replacement);
        if after == before {
            continue;
        }
        if std::fs::write(path, after.as_bytes()).is_ok() {
            changed += 1;
        }
    }
    changed
}

/// Which field of the search box takes typed input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Field {
    #[default]
    Query,
    Replace,
}

/// The project-search feature as a plugin.
#[derive(Default)]
pub(crate) struct ProjectSearchPlugin {
    query: String,
    case_sensitive: bool,
    results: Vec<SearchHit>,
    selected: usize,
    running: bool,
    /// Monotonic run id, embedded in each job's correlation id so a stale result is dropped.
    generation: u64,
    /// The query last actually run — Enter re-runs when it differs, else opens the selection.
    last_run: String,
    /// The replacement text. Empty is meaningful (delete every match), so "should we replace"
    /// is decided by the user pressing the replace key, never by this being non-empty.
    replace: String,
    /// Which field typing goes to (Tab switches).
    field: Field,
    /// Open buffers already replaced while a disk-side replace job is still running, so the
    /// completion message can report one total rather than two halves.
    pending_buffer_replacements: usize,
}

impl ProjectSearchPlugin {
    const ID: &'static str = "project-search";
    const PANEL: &'static str = "search.results";
    const PROMPT: &'static str = "search";

    fn move_selection(&mut self, delta: isize) {
        if self.results.is_empty() {
            return;
        }
        let n = self.results.len() as isize;
        self.selected = (self.selected as isize + delta).rem_euclid(n) as usize;
    }

    /// Open the query box (a key-routing-only prompt) and paint the panel.
    fn open(&mut self, host: &mut dyn Host) {
        // Seed the query from the primary selection.
        if let Some(id) = host.active_doc() {
            if let Some(doc) = host.workspace().documents.get(id) {
                let sel = doc.selections.primary();
                if !sel.is_empty() {
                    self.query = doc.rope().slice(sel.from()..sel.to()).to_string();
                }
            }
        }
        self.results.clear();
        self.selected = 0;
        self.running = false;
        self.last_run.clear();
        self.field = Field::Query;
        host.set_prompt(Prompt::new(Self::ID, Self::PROMPT, PromptPlacement::Panel));
        self.render(host);
    }

    /// Close: drop the query box and clear the results panel.
    fn close(&mut self, host: &mut dyn Host) {
        host.dismiss_prompt();
        host.set_panel(Self::PANEL, PanelContent::default());
    }

    /// Kick off a background search for the current query (tagged with a fresh generation).
    fn run(&mut self, host: &mut dyn Host) {
        if self.query.is_empty() {
            return;
        }
        self.running = true;
        self.results.clear();
        self.selected = 0;
        self.generation += 1;
        self.last_run = self.query.clone();
        let root = host.root().to_path_buf();
        let query = self.query.clone();
        let case = self.case_sensitive;
        let work = Box::new(move || encode(&run_search(&root, &query, case, HIT_CAP)));
        host.spawn_job(format!("search:{}", self.generation), work);
        self.render(host);
    }

    /// The search pattern as a `regex::Regex`, honouring the case toggle. `grep-regex` compiles
    /// the same syntax for searching; this is the replacement-side twin, so `$1` in the
    /// replacement expands against the same captures the search matched.
    fn compiled(&self) -> Option<Regex> {
        RegexBuilder::new(&self.query)
            .case_insensitive(!self.case_sensitive)
            .build()
            .ok()
    }

    /// Replace every match of the last-run query across the project.
    ///
    /// Open buffers and closed files take different paths on purpose. An open buffer is the
    /// truth for its path, so it is edited through a `Transaction` — undoable, and it cannot be
    /// clobbered by the next save. Closed files are rewritten on a worker thread, because a
    /// project-wide replace can touch hundreds of them and the read-modify-write must not run on
    /// the frame loop.
    fn replace_all(&mut self, host: &mut dyn Host) {
        if self.query.is_empty() || self.results.is_empty() {
            return;
        }
        let Some(re) = self.compiled() else {
            host.notify("Replace: the search pattern is not a valid regex".into());
            return;
        };
        // Every distinct file the last search hit, in first-hit order.
        let mut paths: Vec<PathBuf> = Vec::new();
        for hit in &self.results {
            if !paths.contains(&hit.path) {
                paths.push(hit.path.clone());
            }
        }
        // Split by "is this path open in the editor right now".
        let open: Vec<(editor_core::DocId, PathBuf)> = paths
            .iter()
            .filter_map(|p| {
                let id = host
                    .workspace()
                    .documents
                    .iter()
                    .find(|(_, d)| d.path.as_deref() == Some(p.as_path()))
                    .map(|(id, _)| id)?;
                Some((id, p.clone()))
            })
            .collect();
        let on_disk: Vec<PathBuf> = paths
            .iter()
            .filter(|p| !open.iter().any(|(_, o)| o == *p))
            .cloned()
            .collect();

        // Buffers first, synchronously: they are already in memory, and doing them here keeps
        // each file's edit in its own undo entry.
        let mut buffers_changed = 0usize;
        for (id, _) in &open {
            let Some(doc) = host.workspace().documents.get(*id) else {
                continue;
            };
            let text = doc.rope().to_string();
            let mut changes = Vec::new();
            for m in re.find_iter(&text) {
                let at = text[..m.start()].chars().count();
                let matched = m.as_str().to_string();
                let mut inserted = String::new();
                match re.captures(&matched) {
                    Some(caps) => caps.expand(&self.replace, &mut inserted),
                    None => inserted = self.replace.clone(),
                }
                changes.push(Change {
                    at,
                    removed: matched,
                    inserted,
                });
            }
            if changes.is_empty() {
                continue;
            }
            host.apply_transaction(*id, Transaction::from_changes(changes));
            buffers_changed += 1;
        }

        if on_disk.is_empty() {
            host.notify(format!("Replaced in {buffers_changed} open file(s)"));
            self.run(host); // re-search so the panel reflects what is left
            return;
        }
        self.generation += 1;
        let replacement = self.replace.clone();
        let work = Box::new(move || {
            let n = replace_on_disk(&on_disk, &re, &replacement);
            (n as u64).to_le_bytes().to_vec()
        });
        host.spawn_job(format!("replace:{}", self.generation), work);
        self.pending_buffer_replacements = buffers_changed;
        host.notify("Replacing…".into());
    }

    /// Open the selected hit at its line.
    fn open_selected(&self, host: &mut dyn Host) {
        if let Some(hit) = self.results.get(self.selected).cloned() {
            host.open_path_at(&hit.path, hit.line.saturating_sub(1));
        }
    }

    /// Publish the query line + grouped results as a `PanelContent` the app draws.
    fn render(&self, host: &mut dyn Host) {
        let root = host.root().to_path_buf();
        let status = if self.running {
            "searching…".to_string()
        } else {
            format!("{} result(s)", self.results.len())
        };
        let caret = |f: Field| if self.field == f { "▏" } else { "" };
        let mut lines = vec![
            PanelLine::new(vec![Span::new(
                format!("Search:  {}{}  [{status}]", self.query, caret(Field::Query)),
                "title",
            )]),
            PanelLine::new(vec![Span::new(
                format!(
                    "Replace: {}{}   (Tab switches · Alt+A replaces every match)",
                    self.replace,
                    caret(Field::Replace)
                ),
                "dim",
            )]),
        ];
        let mut selected_line = 0;
        let mut last_file: Option<PathBuf> = None;
        for (i, hit) in self.results.iter().enumerate() {
            if last_file.as_deref() != Some(hit.path.as_path()) {
                last_file = Some(hit.path.clone());
                let name = hit
                    .path
                    .strip_prefix(&root)
                    .unwrap_or(&hit.path)
                    .to_string_lossy()
                    .into_owned();
                lines.push(PanelLine::new(vec![Span::new(name, "dir")]));
            }
            let text: String = hit.text.chars().take(120).collect();
            let payload = format!("{}\t{}", hit.path.to_string_lossy(), hit.line);
            if i == self.selected {
                selected_line = lines.len();
            }
            lines.push(
                PanelLine::new(vec![
                    Span::new(format!("  {:>4}: ", hit.line), "dim"),
                    Span::new(text, "text"),
                ])
                .payload(payload),
            );
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

impl Plugin for ProjectSearchPlugin {
    fn id(&self) -> &str {
        Self::ID
    }

    fn contributions(&self) -> Contributions {
        Contributions::builder()
            .command("search.project", "Search: Find in Files")
            .panel(Self::PANEL, "Search", PanelLocation::Bottom)
            .keybinding("ctrl+shift+f", "search.project")
            .build()
    }

    fn run_command(&mut self, command_id: &str, host: &mut dyn Host) -> bool {
        if command_id == "search.project" {
            self.open(host);
            return true;
        }
        false
    }

    fn on_prompt_key(&mut self, prompt_id: &str, key: Key, host: &mut dyn Host) -> bool {
        if prompt_id != Self::PROMPT {
            return false;
        }
        match key.code {
            KeyCode::Esc => self.close(host),
            KeyCode::Up => {
                self.move_selection(-1);
                self.render(host);
            }
            KeyCode::Down => {
                self.move_selection(1);
                self.render(host);
            }
            KeyCode::Tab => {
                self.field = match self.field {
                    Field::Query => Field::Replace,
                    Field::Replace => Field::Query,
                };
                self.render(host);
            }
            // Alt+A mirrors the in-buffer find widget's Replace All, so the chord means the same
            // thing in both search surfaces.
            KeyCode::Char('a' | 'A') if key.alt => self.replace_all(host),
            KeyCode::Backspace => {
                match self.field {
                    Field::Query => self.query.pop(),
                    Field::Replace => self.replace.pop(),
                };
                self.render(host);
            }
            KeyCode::Enter => {
                if self.query != self.last_run {
                    self.run(host);
                } else {
                    self.open_selected(host);
                }
            }
            KeyCode::Char(c) if !key.ctrl && !key.alt => {
                match self.field {
                    Field::Query => self.query.push(c),
                    Field::Replace => self.replace.push(c),
                }
                self.render(host);
            }
            _ => {}
        }
        true
    }

    fn on_event(&mut self, event: &Event, host: &mut dyn Host) {
        let Event::JobComplete { id, payload } = event else {
            return;
        };
        if let Some(gen) = id
            .strip_prefix("replace:")
            .and_then(|g| g.parse::<u64>().ok())
        {
            if gen != self.generation {
                return;
            }
            let files = payload
                .get(..8)
                .and_then(|b| b.try_into().ok())
                .map(u64::from_le_bytes)
                .unwrap_or(0) as usize
                + self.pending_buffer_replacements;
            self.pending_buffer_replacements = 0;
            host.notify(format!("Replaced in {files} file(s)"));
            // Re-run the search so the panel shows what is left rather than stale hits that no
            // longer exist in the files.
            self.run(host);
            return;
        }
        // Only our own jobs, and only the current generation (drop stale results).
        let Some(gen) = id
            .strip_prefix("search:")
            .and_then(|g| g.parse::<u64>().ok())
        else {
            return;
        };
        if gen != self.generation {
            return;
        }
        self.results = decode(payload);
        self.selected = 0;
        self.running = false;
        self.render(host);
    }

    fn on_panel_activate(&mut self, panel_id: &str, payload: &str, host: &mut dyn Host) {
        if panel_id != Self::PANEL {
            return;
        }
        if let Some((path, line)) = payload.rsplit_once('\t') {
            if let Ok(line) = line.parse::<usize>() {
                host.open_path_at(Path::new(path), line.saturating_sub(1));
            }
        }
    }
}

// --- result framing: a length-prefixed binary encoding so a path/line with a tab or newline is
// safe (unlike a `\t`-joined text format). Both sides are plugin code.

fn encode(hits: &[SearchHit]) -> Vec<u8> {
    let mut out = Vec::new();
    for h in hits {
        write_str(&mut out, &h.path.to_string_lossy());
        out.extend_from_slice(&(h.line as u32).to_le_bytes());
        write_str(&mut out, &h.text);
    }
    out
}

fn write_str(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(&(s.len() as u32).to_le_bytes());
    out.extend_from_slice(s.as_bytes());
}

fn decode(bytes: &[u8]) -> Vec<SearchHit> {
    let mut hits = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let Some((path, ni)) = read_str(bytes, i) else {
            break;
        };
        i = ni;
        if i + 4 > bytes.len() {
            break;
        }
        let line =
            u32::from_le_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]) as usize;
        i += 4;
        let Some((text, ni)) = read_str(bytes, i) else {
            break;
        };
        i = ni;
        hits.push(SearchHit {
            path: PathBuf::from(path),
            line,
            text,
        });
    }
    hits
}

/// Read a length-prefixed UTF-8 string at `i`, returning it + the next offset, or `None` if the
/// buffer is truncated or the bytes aren't valid UTF-8.
fn read_str(bytes: &[u8], i: usize) -> Option<(String, usize)> {
    if i + 4 > bytes.len() {
        return None;
    }
    let len = u32::from_le_bytes([bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]]) as usize;
    let start = i + 4;
    let end = start.checked_add(len)?;
    if end > bytes.len() {
        return None;
    }
    let s = std::str::from_utf8(&bytes[start..end]).ok()?.to_string();
    Some((s, end))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_matches_across_files() {
        let n = 424242u32;
        let mut dir = std::env::temp_dir();
        dir.push(format!("lumina_psearch_{}_{}", std::process::id(), n));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "needle here\nother").unwrap();
        std::fs::write(dir.join("b.txt"), "nothing\nNEEDLE again").unwrap();

        let hits = run_search(&dir, "needle", false, 100);
        assert_eq!(hits.len(), 2); // case-insensitive: matches both

        let hits_cs = run_search(&dir, "needle", true, 100);
        assert_eq!(hits_cs.len(), 1); // only lowercase

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn replace_on_disk_rewrites_matches_and_expands_captures() {
        let mut dir = std::env::temp_dir();
        dir.push(format!("lumina_prepl_{}_{}", std::process::id(), 1u32));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("a.txt");
        let b = dir.join("b.txt");
        std::fs::write(&a, "user@host\nplain line\n").unwrap();
        std::fs::write(&b, "no match here\n").unwrap();

        let re = Regex::new(r"(\w+)@(\w+)").unwrap();
        let changed = replace_on_disk(&[a.clone(), b.clone()], &re, "$2.$1");

        assert_eq!(
            changed, 1,
            "only the file that actually matched is rewritten"
        );
        assert_eq!(
            std::fs::read_to_string(&a).unwrap(),
            "host.user\nplain line\n"
        );
        assert_eq!(
            std::fs::read_to_string(&b).unwrap(),
            "no match here\n",
            "a file with no match is left byte-for-byte alone"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// A file that is not valid UTF-8 must be skipped, not mangled — the same rule the open path
    /// follows. Rewriting it would mean decoding lossily and writing U+FFFD back over it.
    #[test]
    fn replace_on_disk_skips_files_it_cannot_round_trip() {
        let mut dir = std::env::temp_dir();
        dir.push(format!("lumina_prepl_{}_{}", std::process::id(), 2u32));
        std::fs::create_dir_all(&dir).unwrap();
        let latin1 = dir.join("latin1.txt");
        let bytes = b"caf\xe9 needle\n";
        std::fs::write(&latin1, bytes).unwrap();

        let re = Regex::new("needle").unwrap();
        let changed = replace_on_disk(std::slice::from_ref(&latin1), &re, "pin");

        assert_eq!(changed, 0);
        assert_eq!(
            std::fs::read(&latin1).unwrap(),
            bytes,
            "the undecodable file is untouched"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn encode_decode_roundtrips_including_tabs_and_newlines() {
        let hits = vec![
            SearchHit {
                path: PathBuf::from("a/b c.rs"),
                line: 3,
                text: "let x = 1;\twith tab".to_string(),
            },
            SearchHit {
                path: PathBuf::from("weird\tname"),
                line: 128,
                text: "line\nwith newline".to_string(),
            },
        ];
        assert_eq!(decode(&encode(&hits)), hits);
        // A truncated buffer decodes to whatever prefix parsed, never panics.
        let mut bytes = encode(&hits);
        bytes.truncate(bytes.len() - 3);
        let _ = decode(&bytes);
    }

    #[test]
    fn move_selection_wraps() {
        let mut p = ProjectSearchPlugin {
            results: vec![
                SearchHit {
                    path: PathBuf::from("x"),
                    line: 1,
                    text: String::new(),
                },
                SearchHit {
                    path: PathBuf::from("y"),
                    line: 2,
                    text: String::new(),
                },
            ],
            ..Default::default()
        };
        p.move_selection(-1);
        assert_eq!(p.selected, 1);
        p.move_selection(1);
        assert_eq!(p.selected, 0);
    }
}
