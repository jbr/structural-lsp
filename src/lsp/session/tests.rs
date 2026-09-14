use super::*;

#[test]
fn path_uri_requires_an_absolute_path() {
    assert!(path_uri(Path::new("/tmp/x/lib.rs")).is_ok());
    assert!(path_uri(Path::new("relative/lib.rs")).is_err());
}

// ---- lifecycle regressions (real rust-analyzer; run with -- --ignored) ----

/// A scratch copy of the fixture crate, so edits and deletions are safe.
fn scratch_fixture(tag: &str) -> std::path::PathBuf {
    let base =
        std::env::temp_dir().join(format!("structural-session-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("src")).unwrap();
    let manifest = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/rust");
    std::fs::copy(format!("{manifest}/Cargo.toml"), base.join("Cargo.toml")).unwrap();
    std::fs::copy(format!("{manifest}/src/lib.rs"), base.join("src/lib.rs")).unwrap();
    base
}

fn events_with_method(session: &Session, method: &str) -> Vec<usize> {
    session
        .client
        .events
        .iter()
        .enumerate()
        .filter(|(_, event)| event.get("method").and_then(Value::as_str) == Some(method))
        .map(|(index, _)| index)
        .collect()
}

/// (B) The settle contract: a bound that expires without observed
/// quiescence returns Pending — the caller's cue to answer with status
/// and issue no semantic query — and a full-bound settle recovers.
///
/// Driven from a cold start, where no `serverStatus` has arrived yet:
/// deterministic, unlike forcing a post-edit `quiescent=false` (r-a's
/// emission timing there is not stable enough to assert against). The
/// tool layer's Pending guards sit at every settle call site.
#[test]
#[ignore = "spawns rust-analyzer; run explicitly with -- --ignored"]
fn settle_reports_pending_within_a_tiny_bound_and_recovers() {
    let root = scratch_fixture("settle-pending");
    let mut session = Session::start(&root).unwrap();

    // Cold start: no serverStatus yet. A zero-width bound cannot observe
    // quiescence, so settle must report Pending, not block or lie.
    let readiness = session.settle(Duration::ZERO).expect("settle");
    assert!(
        matches!(readiness, Readiness::Pending),
        "no status has arrived; a zero bound expires as Pending: {readiness:?}"
    );

    // The full bound waits out the cold start and reports the epoch.
    let readiness = session
        .settle(crate::state::QUIESCE_BOUND)
        .expect("settle with the full bound");
    assert!(
        matches!(readiness, Readiness::Quiescent { .. }),
        "the full bound reaches quiescence: {readiness:?}"
    );

    session.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}

/// (C) Deleting a previously opened document discards the snapshot AND
/// closes it on the server (didClose after the didOpen); recreating the
/// file produces a fresh didOpen — a legal sequence, rather than a
/// duplicate didOpen against a server that never learned the file closed.
#[test]
#[ignore = "spawns rust-analyzer; run explicitly with -- --ignored"]
fn deleting_an_opened_document_closes_it_on_the_server_and_recovers() {
    let root = scratch_fixture("delete-close");
    let mut session = Session::start(&root).unwrap();
    session
        .ensure_ready(crate::state::QUIESCE_BOUND)
        .expect("cold start reaches quiescence");

    let lib_path = root.join("src/lib.rs");
    let original = std::fs::read_to_string(&lib_path).unwrap();
    let uri = path_uri(&lib_path).unwrap();

    let (_uri, fed, _text) = session.refresh_document(&lib_path).unwrap();
    assert!(fed);
    assert!(session.opened.contains_key(&uri));
    let opens_before = events_with_method(&session, "textDocument/didOpen");
    assert_eq!(opens_before.len(), 1);

    // Delete: actionable error, snapshot discarded, didClose sent.
    std::fs::remove_file(&lib_path).unwrap();
    let error = session
        .refresh_document(&lib_path)
        .expect_err("a deleted opened file is an actionable error");
    assert!(
        error
            .to_string()
            .contains("opened earlier but is now missing"),
        "{error}"
    );
    assert!(
        !session.opened.contains_key(&uri),
        "the stale snapshot is discarded"
    );
    let closes = events_with_method(&session, "textDocument/didClose");
    assert_eq!(closes.len(), 1, "exactly one didClose for the discard");
    assert!(
        closes[0] > opens_before[0],
        "the didClose follows the didOpen"
    );

    // Recreate: a fresh didOpen — legal after didClose; the server never
    // saw a duplicate didOpen for a still-open document.
    std::fs::write(&lib_path, &original).unwrap();
    let (_uri, fed, text) = session.refresh_document(&lib_path).unwrap();
    assert!(fed);
    assert_eq!(text, original);
    assert!(session.opened.contains_key(&uri));
    let opens_after = events_with_method(&session, "textDocument/didOpen");
    assert_eq!(opens_after.len(), 2, "one open per server-side open state");
    assert!(
        opens_after[1] > closes[0],
        "the recreated didOpen comes after the didClose"
    );

    session.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}
