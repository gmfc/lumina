//! Background workers and the message channel they report through (plan §5/§6). The main
//! loop drains this channel each tick, so filesystem-watch and project-search results fold
//! into the same single-threaded dispatch path.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender};
use std::time::Duration;

use notify::{RecursiveMode, Watcher};
use notify_debouncer_full::{new_debouncer, DebounceEventResult};

use crate::search::SearchHit;

/// A message from a background worker to the main loop.
pub enum WorkerMsg {
    /// A path under the project root changed on disk (debounced).
    DiskChanged { path: PathBuf },
    /// A project search finished.
    SearchComplete { query: String, hits: Vec<SearchHit> },
}

/// Create the worker channel.
pub fn channel() -> (Sender<WorkerMsg>, Receiver<WorkerMsg>) {
    std::sync::mpsc::channel()
}

/// Watch `root` recursively with an ~80ms debounce, sending a `DiskChanged` per changed
/// path. Returns the debouncer boxed as `Any` — the caller must keep it alive (dropping it
/// stops the watch). Returns `None` if the watcher can't be created (surfaced to the user).
pub fn spawn_watcher(root: PathBuf, tx: Sender<WorkerMsg>) -> Option<Box<dyn std::any::Any>> {
    let mut debouncer = new_debouncer(
        Duration::from_millis(80),
        None,
        move |res: DebounceEventResult| {
            if let Ok(events) = res {
                for event in events {
                    for path in event.paths.iter() {
                        let _ = tx.send(WorkerMsg::DiskChanged { path: path.clone() });
                    }
                }
            }
        },
    )
    .ok()?;
    debouncer
        .watcher()
        .watch(&root, RecursiveMode::Recursive)
        .ok()?;
    Some(Box::new(debouncer))
}

/// Run a project search on a worker thread; the result arrives as `SearchComplete`.
pub fn spawn_search(root: PathBuf, query: String, case_sensitive: bool, tx: Sender<WorkerMsg>) {
    std::thread::spawn(move || {
        let hits = crate::search::run_search(&root, &query, case_sensitive, 2000);
        let _ = tx.send(WorkerMsg::SearchComplete { query, hits });
    });
}
