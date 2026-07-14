//! Connection lifecycle: the initialize handshake, the crash → exit → breaker → backoff → restart
//! policy, and the per-document `didClose` bookkeeping a crash or close must prune.
use super::*;

#[test]
fn init_response_stores_caps_and_becomes_ready() {
    // The awaited InitializeResult transitions the connection to Running with parsed caps,
    // and produces no user-facing event (the handshake is internal).
    let mut mgr = manager();
    mgr.state
        .insert("rust".into(), ClientState::Initializing { init_id: 1 });
    let caps = serde_json::json!({ "capabilities": { "hoverProvider": true } });
    feed(
        &mgr,
        "rust",
        Incoming::Response {
            id: 1,
            result: caps,
            error: None,
        },
    );
    assert!(mgr.poll().is_empty());
    assert!(mgr.is_ready("rust"));
    assert!(mgr.request_allowed("rust", Cap::Hover));
    assert!(!mgr.request_allowed("rust", Cap::Completion));
}

#[test]
fn restart_backoff_is_exponential_and_capped() {
    assert_eq!(restart_backoff(1), Duration::from_millis(250));
    assert_eq!(restart_backoff(2), Duration::from_millis(500));
    assert_eq!(restart_backoff(3), Duration::from_millis(1000));
    assert_eq!(restart_backoff(4), Duration::from_millis(2000));
    assert_eq!(restart_backoff(5), Duration::from_millis(4000));
    assert_eq!(restart_backoff(9), Duration::from_millis(4000)); // capped
}

#[test]
fn breaker_trips_only_within_the_window() {
    let now = Instant::now();
    // Five crashes inside the window → tripped.
    let recent: Vec<Instant> = (0..5).map(|i| now - Duration::from_secs(i * 10)).collect();
    assert!(breaker_tripped(&recent, now));
    // Four recent + one ancient (outside the window) → not tripped.
    let mut spread = recent[..4].to_vec();
    spread.push(now - Duration::from_secs(600));
    assert!(!breaker_tripped(&spread, now));
}

#[test]
fn server_exit_fails_pending_and_clears_diagnostics() {
    let mut mgr = manager();
    mgr.state
        .insert("rust".into(), ClientState::Running(ServerCaps::default()));
    mgr.pending.insert(("rust".into(), 5), pend(Pending::Hover));
    mgr.published
        .entry("rust".into())
        .or_default()
        .insert("file:///a.rs".into());
    feed_exit(&mgr, "rust");
    let events = mgr.poll();

    assert!(!mgr.state.contains_key("rust"), "state not torn down");
    assert!(mgr.pending.is_empty(), "pending not failed locally");
    assert!(
            events
                .iter()
                .any(|e| matches!(e, LspEvent::Diagnostics(u) if u.uri == "file:///a.rs" && u.diagnostics.is_empty())),
            "stale diagnostics not cleared"
        );
    assert!(events
        .iter()
        .any(|e| matches!(e, LspEvent::ServerExited { lang } if lang == "rust")));
    assert!(events.iter().any(|e| matches!(e, LspEvent::Error(_))));
    // First crash → scheduled for restart, breaker not tripped.
    assert!(mgr.restart_after.contains_key("rust"));
    assert!(!mgr.failed.contains_key("rust"));
}

#[test]
fn did_close_only_for_opened_docs_and_prunes_bookkeeping() {
    let mut mgr = manager();
    let uri = uri_for(Path::new("/x.rs"));
    // Never opened this session → no stray close, nothing to prune (no panic).
    mgr.did_close(Path::new("/x.rs"), "rust");
    assert!(mgr.open_docs.get("rust").is_none_or(|s| s.is_empty()));

    // Simulate an opened doc with diagnostics (as did_open / a publish would record).
    mgr.open_docs
        .entry("rust".into())
        .or_default()
        .insert(uri.clone());
    mgr.published
        .entry("rust".into())
        .or_default()
        .insert(uri.clone());
    mgr.did_close(Path::new("/x.rs"), "rust");
    // Closing removes it from the attach set and the published set (no unbounded growth).
    assert!(mgr.open_docs.get("rust").is_none_or(|s| !s.contains(&uri)));
    assert!(mgr.published.get("rust").is_none_or(|s| !s.contains(&uri)));
}

#[test]
fn breaker_trips_after_repeated_crashes() {
    let mut mgr = manager();
    for _ in 0..CRASH_LIMIT {
        feed_exit(&mgr, "rust");
    }
    let events = mgr.poll();
    assert!(
        mgr.failed.contains_key("rust"),
        "breaker did not trip after {CRASH_LIMIT} crashes"
    );
    assert!(
        !mgr.restart_after.contains_key("rust"),
        "should not be scheduled to restart"
    );
    assert!(events
        .iter()
        .any(|e| matches!(e, LspEvent::Error(m) if m.contains("not restarting"))));
}
