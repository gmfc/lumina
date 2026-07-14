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

#[test]
fn discovery_is_off_by_default_and_overrides_always_win() {
    // A bare manager is inert: discovery off, no overrides → the layer stays disabled and nothing
    // resolves (so a test that opens a `.rs` file never spawns a real server).
    let mut mgr = manager();
    assert!(!mgr.is_enabled());
    assert_eq!(mgr.resolve_server("rust"), None);

    // An explicit `[lsp]` override is honored verbatim and flips the manager on, even with
    // discovery off — the override is the sole candidate and wins over the registry.
    let mut over = LspManager::new(
        Path::new("/tmp"),
        std::collections::HashMap::from([(
            "rust".to_string(),
            vec!["my-ra".to_string(), "--flag".to_string()],
        )]),
        "test".into(),
    );
    assert!(over.is_enabled());
    assert_eq!(
        over.resolve_server("rust"),
        Some(vec!["my-ra".into(), "--flag".into()])
    );
    // A language with neither an override nor discovery resolves to nothing.
    assert_eq!(over.resolve_server("python"), None);
}

#[test]
fn discovery_enables_the_layer_but_unknown_languages_stay_unresolved() {
    let mut mgr = manager();
    mgr.enable_discovery();
    assert!(mgr.is_enabled(), "discovery turns the LSP layer on");
    // A language the registry doesn't know never resolves, whatever is on `$PATH`.
    assert_eq!(mgr.resolve_server("cobol"), None);
    // Turning discovery back off makes a bare manager inert again.
    mgr.disable_discovery();
    assert!(!mgr.is_enabled());
}

#[test]
fn discovery_probes_the_registry_against_path_and_memoizes() {
    // With discovery on, a *known* language runs the registry candidate probe against the real
    // `$PATH`. What resolves depends on the machine, so assert only that it runs without panicking
    // (exercising `probe_server`/`first_installed`/`program_on_path`) and that the result is
    // memoized — a second call returns the same thing from the cache.
    let mut mgr = manager();
    mgr.enable_discovery();
    let first = mgr.resolve_server("go");
    let second = mgr.resolve_server("go");
    assert_eq!(first, second, "resolution is memoized across calls");
    // `rust` also drives the probe (rust-analyzer may or may not be installed here).
    let _ = mgr.resolve_server("rust");
}

#[test]
fn health_tag_reflects_the_active_languages_connection_state() {
    let mut mgr = manager();
    // No language, or a language with no connection → empty (footer shows nothing).
    assert_eq!(mgr.health_tag_for(None), "");
    assert_eq!(mgr.health_tag_for(Some("rust")), "");
    // Handshake in progress → starting (spinner).
    mgr.state
        .insert("rust".into(), ClientState::Initializing { init_id: 1 });
    assert_eq!(mgr.health_tag_for(Some("rust")), "starting");
    // Serving → ready.
    mgr.state
        .insert("rust".into(), ClientState::Running(ServerCaps::default()));
    assert_eq!(mgr.health_tag_for(Some("rust")), "ready");
    // Crashed / spawn-failed (no state entry, in the `failed` set) → error.
    mgr.state.remove("rust");
    mgr.failed.insert("rust".into(), ());
    assert_eq!(mgr.health_tag_for(Some("rust")), "error");
    // A different, unconnected language is still empty.
    assert_eq!(mgr.health_tag_for(Some("go")), "");
}

#[test]
fn spawn_failure_records_an_error_and_marks_the_language_crashed() {
    // An override pointing at a nonexistent binary fails to spawn: the language is marked failed
    // and the error is recorded for the LSP panel's status row.
    let mut mgr = LspManager::new(
        std::path::Path::new("/tmp"),
        std::collections::HashMap::from([(
            "rust".to_string(),
            vec!["/no/such/lumina-binary-xyz".to_string()],
        )]),
        "test".into(),
    );
    assert!(!mgr.ensure_started("rust"), "spawn fails, no connection");
    let rust = mgr
        .status_rows()
        .into_iter()
        .find(|r| r.lang == "rust")
        .unwrap();
    assert_eq!(rust.state, crate::lsp::LangState::Crashed);
    assert!(rust.error.unwrap().contains("failed to start"));
}

#[test]
fn init_failure_records_the_error() {
    // An error response to `initialize` drops the connection and records the message.
    let mut mgr = manager();
    mgr.state
        .insert("rust".into(), ClientState::Initializing { init_id: 1 });
    let mut out = Vec::new();
    mgr.complete_handshake(
        "rust",
        serde_json::Value::Null,
        Some(ResponseError {
            code: -1,
            message: "nope".into(),
        }),
        &mut out,
    );
    assert!(mgr.failed.contains_key("rust"));
    let rust = mgr
        .status_rows()
        .into_iter()
        .find(|r| r.lang == "rust")
        .unwrap();
    assert_eq!(rust.error.as_deref(), Some("initialize failed: nope"));
}
