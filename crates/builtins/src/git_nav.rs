//! Git change navigation, implemented **as a plugin** (invariant #3).
//!
//! Jump the caret to the next (`Alt+J`) or previous (`Alt+K`) git hunk, wrapping around. The
//! change map itself is computed off-thread by the app (shelling out to `git`); this plugin only
//! *navigates* it, reading the changed-line set through [`Host::changed_lines`] and moving the
//! caret through [`Host::set_selections`]. It never touches the filesystem or `git` directly.

use std::collections::BTreeSet;

use editor_core::{Document, Selection, Selections};
use editor_plugin::{Contributions, Host, Plugin};

pub struct GitNavPlugin;

impl Plugin for GitNavPlugin {
    fn id(&self) -> &str {
        "git-nav"
    }

    fn contributions(&self) -> Contributions {
        Contributions::builder()
            .command("git.nextHunk", "Go: Next Change")
            .command("git.prevHunk", "Go: Previous Change")
            .build()
    }

    fn run_command(&mut self, command_id: &str, host: &mut dyn Host) -> bool {
        let dir = match command_id {
            "git.nextHunk" => 1isize,
            "git.prevHunk" => -1,
            _ => return false,
        };
        let Some(id) = host.active_doc() else {
            return true;
        };
        let changed = host.changed_lines(id);
        let new_sels = host
            .workspace()
            .documents
            .get(id)
            .and_then(|doc| goto_hunk(doc, &changed, dir));
        if let Some(sels) = new_sels {
            host.set_selections(id, sels);
        }
        true
    }
}

/// Move the caret to the start of the next (`dir > 0`) or previous git hunk, wrapping around.
/// A hunk starts at a changed line whose predecessor is unchanged.
fn goto_hunk(doc: &Document, changed: &[usize], dir: isize) -> Option<Selections> {
    if changed.is_empty() {
        return None;
    }
    let set: BTreeSet<usize> = changed.iter().copied().collect();
    let mut starts: Vec<usize> = set
        .iter()
        .copied()
        .filter(|&l| l == 0 || !set.contains(&(l - 1)))
        .collect();
    starts.sort_unstable();
    if starts.is_empty() {
        return None;
    }
    let cur = doc.char_to_line(doc.selections.primary().head);
    let target = if dir > 0 {
        starts
            .iter()
            .copied()
            .find(|&l| l > cur)
            .unwrap_or(starts[0])
    } else {
        starts
            .iter()
            .rev()
            .copied()
            .find(|&l| l < cur)
            .unwrap_or_else(|| *starts.last().unwrap())
    };
    Some(Selections::single(Selection::caret(
        doc.line_to_char(target),
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn navigates_and_wraps_between_hunk_starts() {
        // Hunks starting at lines 1 and 5 (contiguous changes 5,6 collapse to one start at 5).
        let doc = Document::from_str("l0\nl1\nl2\nl3\nl4\nl5\nl6\nl7");
        let changed = [1usize, 5, 6];

        // From line 0, next hunk is line 1.
        let s = goto_hunk(&doc, &changed, 1).unwrap();
        assert_eq!(doc.char_to_line(s.primary().head), 1);

        // From line 2 (place caret there), next is line 5; prev is line 1.
        let mut doc2 = Document::from_str("l0\nl1\nl2\nl3\nl4\nl5\nl6\nl7");
        doc2.set_caret(doc2.line_to_char(2));
        assert_eq!(
            doc2.char_to_line(goto_hunk(&doc2, &changed, 1).unwrap().primary().head),
            5
        );
        assert_eq!(
            doc2.char_to_line(goto_hunk(&doc2, &changed, -1).unwrap().primary().head),
            1
        );

        // From the last hunk, next wraps to the first.
        doc2.set_caret(doc2.line_to_char(5));
        assert_eq!(
            doc2.char_to_line(goto_hunk(&doc2, &changed, 1).unwrap().primary().head),
            1
        );
    }

    #[test]
    fn no_changes_is_a_noop() {
        let doc = Document::from_str("a\nb\nc");
        assert!(goto_hunk(&doc, &[], 1).is_none());
    }
}
