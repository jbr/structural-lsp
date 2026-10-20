use super::*;
use crate::lsp::adapter::rust_analyzer::RustAnalyzer;
use serde_json::json;

#[cfg(unix)]
mod spawning;

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
    std::fs::create_dir_all(&base).unwrap();
    crate::fixtures::get("rust")
        .expect("the rust fixture")
        .materialize(&base)
        .unwrap();
    base
}

fn events_with_method(session: &LspServer, method: &str) -> Vec<usize> {
    session
        .events()
        .unwrap()
        .iter()
        .enumerate()
        .filter(|(_, event)| event.get("method").and_then(Value::as_str) == Some(method))
        .map(|(index, _)| index)
        .collect()
}

/// A cold start is not ready until rust-analyzer reports quiescence; the
/// wait blocks through the initial load and then the snapshot agrees.
#[test]
#[ignore = "spawns rust-analyzer; run explicitly with -- --ignored"]
fn cold_start_waits_for_quiescence() {
    let root = scratch_fixture("cold-start");
    let mut session = LspServer::start(std::sync::Arc::new(RustAnalyzer), &root).unwrap();
    session
        .wait_ready()
        .expect("the cold start settles within the backstop");
    let snapshot = session.snapshot().unwrap();
    assert!(snapshot.ready, "{snapshot:?}");
    assert_eq!(snapshot.open_progress_tokens, 0, "{snapshot:?}");
    // The evidence is in the log: at least one quiescent status.
    assert!(
        session.events().unwrap().iter().any(|event| {
            event["method"] == "experimental/serverStatus"
                && event.pointer("/params/quiescent") == Some(&json!(true))
        }),
        "a quiescent serverStatus was observed"
    );
    session.shutdown();
    let _ = std::fs::remove_dir_all(&root);
}

/// Deleting a previously opened document discards the snapshot AND closes
/// it on the server (didClose after the didOpen); recreating the file
/// produces a fresh didOpen — a legal sequence, rather than a duplicate
/// didOpen against a server that never learned the file closed.
#[test]
#[ignore = "spawns rust-analyzer; run explicitly with -- --ignored"]
fn deleting_an_opened_document_closes_it_on_the_server_and_recovers() {
    let root = scratch_fixture("delete-close");
    let mut session = LspServer::start(std::sync::Arc::new(RustAnalyzer), &root).unwrap();
    session.wait_ready().expect("cold start reaches quiescence");

    let lib_path = root.join("src/lib.rs");
    let original = std::fs::read_to_string(&lib_path).unwrap();
    let uri = path_uri(&lib_path).unwrap();

    let synced = session.sync(&lib_path).unwrap();
    assert!(synced.fed);
    assert!(session.opened.contains_key(&uri));
    let opens_before = events_with_method(&session, "textDocument/didOpen");
    assert_eq!(opens_before.len(), 1);
    assert!(
        !session.sync(&lib_path).unwrap().fed,
        "unchanged: nothing sent"
    );

    // Delete: actionable error, snapshot discarded, didClose sent.
    std::fs::remove_file(&lib_path).unwrap();
    let error = session
        .sync(&lib_path)
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
    let synced = session.sync(&lib_path).unwrap();
    assert!(synced.fed);
    assert_eq!(synced.text, original);
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
