//! Project-wide search (plan §6): the ripgrep engine as a library
//! (`grep-searcher` + `grep-regex` + `ignore`). Runs on a worker thread; results stream
//! back through the worker channel and render in a bottom results panel.

use std::path::{Path, PathBuf};

use grep_regex::RegexMatcherBuilder;
use grep_searcher::sinks::UTF8;
use grep_searcher::Searcher;
use ignore::WalkBuilder;

/// One match: a file, a 1-based line number, and the matching line's text.
#[derive(Debug, Clone)]
pub struct SearchHit {
    pub path: PathBuf,
    pub line: usize,
    pub text: String,
}

/// Live project-search state (the query box + streamed results).
#[derive(Default)]
pub struct SearchState {
    pub query: String,
    pub case_sensitive: bool,
    pub results: Vec<SearchHit>,
    pub selected: usize,
    pub running: bool,
}

impl SearchState {
    pub fn move_selection(&mut self, delta: isize) {
        if self.results.is_empty() {
            return;
        }
        let n = self.results.len() as isize;
        self.selected = (self.selected as isize + delta).rem_euclid(n) as usize;
    }

    pub fn selected_hit(&self) -> Option<&SearchHit> {
        self.results.get(self.selected)
    }
}

/// Run a project search under `root` for `pattern`. Blocking — call on a worker thread.
/// Caps total hits to keep the panel bounded (logs the cap by returning `truncated`).
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    static C: AtomicU32 = AtomicU32::new(0);

    #[test]
    fn finds_matches_across_files() {
        let n = C.fetch_add(1, Ordering::SeqCst);
        let mut dir = std::env::temp_dir();
        dir.push(format!("lumina_search_{}_{}", std::process::id(), n));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "needle here\nother").unwrap();
        std::fs::write(dir.join("b.txt"), "nothing\nNEEDLE again").unwrap();

        let hits = run_search(&dir, "needle", false, 100);
        assert_eq!(hits.len(), 2); // case-insensitive: matches both

        let hits_cs = run_search(&dir, "needle", true, 100);
        assert_eq!(hits_cs.len(), 1); // only lowercase

        std::fs::remove_dir_all(&dir).ok();
    }
}
