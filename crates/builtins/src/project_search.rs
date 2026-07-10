//! Project-wide search, implemented **as a plugin** (invariant #3).
//!
//! The ripgrep engine (`grep-searcher` + `grep-regex` + `ignore`) runs off the main thread via
//! the background-job port ([`Host::spawn_job`]); results come back as [`Event::JobComplete`],
//! tagged with a generation so a stale search is dropped. The plugin owns the whole search model
//! and reaches the editor only through `Host`: the query box is the generic [`Prompt`] port (a
//! `Panel`-placement prompt used for key routing only), results are published as a `PanelContent`
//! the app draws in the bottom dock, and a hit opens through [`Host::open_path_at`].

use std::path::{Path, PathBuf};

use editor_plugin::{
    Contributions, Event, Host, Key, KeyCode, PanelContent, PanelLine, PanelLocation, Plugin,
    Prompt, PromptPlacement, Span,
};
use grep_regex::RegexMatcherBuilder;
use grep_searcher::sinks::UTF8;
use grep_searcher::Searcher;
use ignore::WalkBuilder;

/// Total hits kept, so a broad query on a big tree stays bounded.
const HIT_CAP: usize = 2000;

/// One match: a file, a 1-based line number, and the matching line's text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    pub path: PathBuf,
    pub line: usize,
    pub text: String,
}

/// Run a project search under `root` for `pattern`. Blocking — call on a worker thread.
pub fn run_search(root: &Path, pattern: &str, case_sensitive: bool, cap: usize) -> Vec<SearchHit> {
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

/// The project-search feature as a plugin.
#[derive(Default)]
pub struct ProjectSearchPlugin {
    query: String,
    case_sensitive: bool,
    results: Vec<SearchHit>,
    selected: usize,
    running: bool,
    /// Monotonic run id, embedded in each job's correlation id so a stale result is dropped.
    generation: u64,
    /// The query last actually run — Enter re-runs when it differs, else opens the selection.
    last_run: String,
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
        let mut lines = vec![PanelLine::new(vec![Span::new(
            format!("Search: {}▏  [{status}]", self.query),
            "title",
        )])];
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
            KeyCode::Backspace => {
                self.query.pop();
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
                self.query.push(c);
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
