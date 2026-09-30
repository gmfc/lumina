//! CI-gated microbench smoke for Performance §2.
//!
//! Measures open / scroll / keystroke on synthetic 1k / 10k / 100k LOC buffers and asserts
//! against regress ceilings. Release-mode only in spirit (CI runs --release); thresholds are
//! deliberately loose for shared runners while still catching order-of-magnitude regressions.
//!
//! Run locally:
//!   cargo test -p editor-core --release --test perf_smoke

use std::hint::black_box;
use std::time::{Duration, Instant};

use editor_core::view::ViewState;
use editor_core::{Document, Transaction};

fn corpus(lines: usize) -> String {
    let mut body = String::with_capacity(lines.saturating_mul(72));
    for i in 0..lines {
        body.push_str(&format!(
            "    let value_{i} = compute(x, y) + offset; // row {i}\n"
        ));
    }
    body
}

fn assert_under(label: &str, elapsed: Duration, ceiling: Duration) {
    assert!(
        elapsed <= ceiling,
        "{label}: {elapsed:?} exceeded ceiling {ceiling:?}"
    );
}

fn measure_open(lines: usize) -> (Document, Duration) {
    let body = corpus(lines);
    let t0 = Instant::now();
    let doc = Document::from_str(black_box(&body));
    let elapsed = t0.elapsed();
    // Ropey counts a trailing empty line after a final `\n`, so N content lines → N+1.
    assert!(
        doc.len_lines() >= lines,
        "expected at least {lines} lines, got {}",
        doc.len_lines()
    );
    let _ = black_box(doc.len_chars());
    (doc, elapsed)
}

fn measure_scroll(doc: &mut Document, steps: usize) -> Duration {
    let height = 40usize;
    let lines = doc.len_lines();
    let t0 = Instant::now();
    for i in 0..steps {
        let target = (i * 17) % lines.max(1);
        doc.view.scroll_to_line(black_box(target), height);
        black_box(doc.view.scroll_line);
    }
    t0.elapsed()
}

fn measure_keystroke(doc: &mut Document, strokes: usize) -> Duration {
    // Insert near the middle so the rope pays for a realistic split, not append-only.
    let mut at = doc.len_chars() / 2;
    let t0 = Instant::now();
    for i in 0..strokes {
        let ch = if i % 40 == 39 { "\n" } else { "x" };
        let txn = Transaction::insert(doc, at, ch);
        let _inv = txn.apply(doc);
        at += 1;
        black_box(doc.revision);
    }
    t0.elapsed()
}

/// Ceilings sized for GitHub-hosted runners under load. Tighten once a baseline is published.
mod ceilings {
    use super::Duration;
    pub const OPEN_1K: Duration = Duration::from_millis(200);
    pub const OPEN_10K: Duration = Duration::from_millis(800);
    pub const OPEN_100K: Duration = Duration::from_millis(4_000);
    pub const SCROLL_1K: Duration = Duration::from_millis(50);
    pub const SCROLL_10K: Duration = Duration::from_millis(80);
    pub const SCROLL_100K: Duration = Duration::from_millis(150);
    pub const KEY_1K: Duration = Duration::from_millis(150);
    pub const KEY_10K: Duration = Duration::from_millis(250);
    pub const KEY_100K: Duration = Duration::from_millis(500);
}

fn smoke_size(lines: usize, open_c: Duration, scroll_c: Duration, key_c: Duration) {
    let (mut doc, open) = measure_open(lines);
    assert_under(&format!("open {lines} LOC"), open, open_c);

    let scroll = measure_scroll(&mut doc, 500);
    assert_under(&format!("scroll {lines} LOC"), scroll, scroll_c);

    let key = measure_keystroke(&mut doc, 200);
    assert_under(&format!("keystroke {lines} LOC"), key, key_c);
}

#[test]
fn smoke_1k_loc_open_scroll_keystroke() {
    smoke_size(
        1_000,
        ceilings::OPEN_1K,
        ceilings::SCROLL_1K,
        ceilings::KEY_1K,
    );
}

#[test]
fn smoke_10k_loc_open_scroll_keystroke() {
    smoke_size(
        10_000,
        ceilings::OPEN_10K,
        ceilings::SCROLL_10K,
        ceilings::KEY_10K,
    );
}

#[test]
fn smoke_100k_loc_open_scroll_keystroke() {
    smoke_size(
        100_000,
        ceilings::OPEN_100K,
        ceilings::SCROLL_100K,
        ceilings::KEY_100K,
    );
}

#[test]
fn view_state_scroll_moves_for_smoke() {
    // Tiny sanity so the smoke suite also covers ViewState without a full corpus.
    let mut view = ViewState::default();
    view.scroll_to_line(100, 40);
    assert!(view.scroll_line > 0);
}
