//! Background workers and the message channel they report through (plan §5/§6). The main
//! loop drains this channel each tick, so filesystem-watch and project-search results fold
//! into the same single-threaded dispatch path.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use std::time::Duration;

use notify::{PollWatcher, RecursiveMode, Watcher};
use notify_debouncer_full::{new_debouncer, new_debouncer_opt, DebounceEventResult, FileIdMap};

use crate::search::SearchHit;

/// A message from a background worker to the main loop.
pub enum WorkerMsg {
    /// A path under the project root changed on disk (debounced).
    DiskChanged { path: PathBuf },
    /// A project search finished.
    SearchComplete { query: String, hits: Vec<SearchHit> },
    /// A git diff computation finished for `path` (plan §4.1).
    GitStatus {
        path: PathBuf,
        statuses: crate::git::LineStatuses,
    },
    /// A chunk of output from a terminal's PTY reader thread.
    TerminalOutput { id: u64, bytes: Vec<u8> },
    /// A terminal's shell process exited (its reader thread reached EOF).
    TerminalExited { id: u64 },
}

/// Create the worker channel.
pub fn channel() -> (Sender<WorkerMsg>, Receiver<WorkerMsg>) {
    std::sync::mpsc::channel()
}

/// Build a debounced-event handler that forwards each changed path to the main loop.
fn make_handler(tx: Sender<WorkerMsg>) -> impl FnMut(DebounceEventResult) + Send + 'static {
    move |res: DebounceEventResult| {
        if let Ok(events) = res {
            for event in events {
                for path in event.paths.iter() {
                    let _ = tx.send(WorkerMsg::DiskChanged { path: path.clone() });
                }
            }
        }
    }
}

/// Watch `root` recursively (plus an optional `extra` dir, non-recursively, for the config
/// file) with an ~80ms debounce, sending a `DiskChanged` per changed path. When `poll` is set,
/// use notify's stat-polling `PollWatcher` — the reliable fallback for devcontainer bind
/// mounts / network filesystems where inotify/FSEvents don't fire (plan §6 caveat).
///
/// Returns the debouncer boxed as `Any` — the caller must keep it alive (dropping it stops the
/// watch). Returns `None` if the watcher can't be created (surfaced to the user).
pub fn spawn_watcher(
    root: PathBuf,
    extra: Option<PathBuf>,
    poll: bool,
    tx: Sender<WorkerMsg>,
) -> Option<Box<dyn std::any::Any>> {
    if poll {
        let cfg = notify::Config::default().with_poll_interval(Duration::from_millis(250));
        let mut debouncer = new_debouncer_opt::<_, PollWatcher, FileIdMap>(
            Duration::from_millis(200),
            None,
            make_handler(tx),
            FileIdMap::new(),
            cfg,
        )
        .ok()?;
        let w = debouncer.watcher();
        w.watch(&root, RecursiveMode::Recursive).ok()?;
        if let Some(dir) = &extra {
            let _ = w.watch(dir, RecursiveMode::NonRecursive);
        }
        Some(Box::new(debouncer))
    } else {
        let mut debouncer =
            new_debouncer(Duration::from_millis(80), None, make_handler(tx)).ok()?;
        let w = debouncer.watcher();
        w.watch(&root, RecursiveMode::Recursive).ok()?;
        if let Some(dir) = &extra {
            let _ = w.watch(dir, RecursiveMode::NonRecursive);
        }
        Some(Box::new(debouncer))
    }
}

/// Run a project search on a worker thread; the result arrives as `SearchComplete`.
pub fn spawn_search(root: PathBuf, query: String, case_sensitive: bool, tx: Sender<WorkerMsg>) {
    std::thread::spawn(move || {
        let hits = crate::search::run_search(&root, &query, case_sensitive, 2000);
        let _ = tx.send(WorkerMsg::SearchComplete { query, hits });
    });
}

/// Compute a file's git change map off the main thread; result arrives as `GitStatus`
/// (plan §4.1 — git compute never blocks the event loop).
pub fn spawn_git(root: PathBuf, path: PathBuf, tx: Sender<WorkerMsg>) {
    std::thread::spawn(move || {
        let statuses = crate::git::compute(&root, &path);
        let _ = tx.send(WorkerMsg::GitStatus { path, statuses });
    });
}
