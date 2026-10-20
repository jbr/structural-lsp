//! The readiness generation: a semantic answer that arrives while work
//! begun after its write is outstanding is set aside and asked again; one
//! from a server that stayed idle, or had settled again, is accepted as it
//! came (docs/correctness.md).
//!
//! Every case is ordered by the mock's single output stream, never by time:
//! the signal that opens the work is written before the response, so the
//! owner has applied it by the time the response is routed.

use super::*;

/// How many times `method` was written to the server.
fn written(handle: &ServerHandle, method: &str) -> usize {
    handle
        .capture_events()
        .unwrap()
        .iter()
        .filter(|event| {
            event.get("method").and_then(Value::as_str) == Some(method) && event.get("id").is_some()
        })
        .count()
}

#[test]
fn an_answer_straddling_a_progress_begin_is_set_aside_and_asked_again() {
    needs_python!();
    let mock = Mock::new("straddle");
    let handle = mock.spawn(test_settings(), Box::new(StandardIdle));
    initialize(&handle);
    assert!(handle.snapshot().unwrap().ready, "idle before the query");

    // Ready when written; a `begin` arrives before the answer: the answer
    // was produced while work was outstanding, and is not the one returned.
    let answer = handle
        .query("test/straddle", json!({ "tag": "a" }))
        .unwrap();
    assert_eq!(answer, json!("settled"));
    let snapshot = handle.snapshot().unwrap();
    assert_eq!(snapshot.reasked, 1, "{snapshot:?}");
    assert_eq!(snapshot.generation, 1, "{snapshot:?}");
    assert_eq!(written(&handle, "test/straddle"), 2, "written, then again");
    assert_eq!(snapshot.pending_operations, 0, "{snapshot:?}");

    // The same exchange on the unguarded path returns the straddling answer:
    // the guard, not the mock, is what made the difference.
    let unguarded = handle
        .request("test/straddle", json!({ "tag": "b" }))
        .unwrap();
    assert_eq!(unguarded, json!("unsettled"));
    handle.shutdown().unwrap();
}

#[test]
fn a_counted_create_and_a_policy_turning_are_starts_of_work_and_a_report_is_not() {
    needs_python!();
    let mock = Mock::new("straddle-signals");
    let gate = Arc::new(AtomicBool::new(true));
    let handle = mock.spawn(test_settings(), Box::new(Gate(Arc::clone(&gate))));
    initialize(&handle);

    for how in ["create", "gate"] {
        let answer = handle
            .query("test/straddle", json!({ "tag": how, "how": how }))
            .unwrap();
        assert_eq!(answer, json!("settled"), "work opened by {how}");
    }
    let before = handle.snapshot().unwrap();
    assert_eq!(before.reasked, 2, "{before:?}");

    // A report on no open token proves the server alive and starts nothing:
    // the answer is accepted as it came.
    let answer = handle
        .query("test/straddle", json!({ "tag": "r", "how": "report" }))
        .unwrap();
    assert_eq!(answer, json!("unsettled"));
    let after = handle.snapshot().unwrap();
    assert_eq!(after.reasked, before.reasked, "{after:?}");
    assert_eq!(after.generation, before.generation, "{after:?}");
    handle.shutdown().unwrap();
}

/// pyright opens and ends a "Finding references" token for every
/// `references` it answers, before the answer. Work that has ended by the
/// time the answer arrives does not set it aside; if it did, every such
/// request would be asked forever — which the real-server suite showed.
#[test]
fn work_that_ended_before_the_answer_arrived_does_not_set_it_aside() {
    needs_python!();
    let mock = Mock::new("scoped-work");
    let handle = mock.spawn(test_settings(), Box::new(StandardIdle));
    initialize(&handle);
    let answer = handle
        .query("test/straddle", json!({ "tag": "s", "how": "scoped" }))
        .unwrap();
    assert_eq!(answer, json!("unsettled"), "the answer as it came");
    let snapshot = handle.snapshot().unwrap();
    assert_eq!(snapshot.generation, 1, "the work was seen: {snapshot:?}");
    assert_eq!(snapshot.reasked, 0, "{snapshot:?}");
    assert_eq!(written(&handle, "test/straddle"), 1);
    handle.shutdown().unwrap();
}

#[test]
fn an_idle_server_is_asked_once() {
    needs_python!();
    let mock = Mock::new("idle-query");
    let handle = mock.spawn(test_settings(), Box::new(StandardIdle));
    initialize(&handle);
    for _ in 0..3 {
        assert_eq!(
            handle.query("test/nullresult", json!({})).unwrap(),
            Value::Null,
            "a null answer from an idle server is the server's answer"
        );
    }
    let snapshot = handle.snapshot().unwrap();
    assert_eq!((snapshot.reasked, snapshot.generation), (0, 0));
    assert_eq!(written(&handle, "test/nullresult"), 3);
    handle.shutdown().unwrap();
}

#[test]
fn a_semantic_request_is_held_until_the_server_is_ready() {
    needs_python!();
    let mock = Mock::new("held-query");
    let gate = Arc::new(AtomicBool::new(false));
    let handle = mock.spawn(test_settings(), Box::new(Gate(Arc::clone(&gate))));
    initialize(&handle);
    assert!(!handle.snapshot().unwrap().ready);

    let asker = thread::spawn({
        let handle = handle.clone_for_test();
        move || handle.query("test/straddle", json!({ "tag": "held", "how": "report" }))
    });
    // The query is admitted and held: an operation, and no frame on the wire.
    // The bound only keeps a failure from hanging.
    let deadline = Instant::now() + Duration::from_secs(10);
    while handle.snapshot().unwrap().pending_operations == 0 && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(handle.snapshot().unwrap().pending_operations, 1);
    assert_eq!(written(&handle, "test/straddle"), 0, "held, not written");

    handle
        .request("test/gate", json!({ "open": true }))
        .unwrap();
    assert_eq!(asker.join().unwrap().unwrap(), json!("unsettled"));
    // Written once, after the gate opened.
    let events = handle.capture_events().unwrap();
    let opened = events
        .iter()
        .position(|event| {
            event.get("method").and_then(Value::as_str) == Some("test/gate")
                && event.pointer("/params/open") == Some(&json!(true))
                && event.get("id").is_none()
        })
        .expect("the gate notification");
    let sent = events
        .iter()
        .position(|event| {
            event.get("method").and_then(Value::as_str) == Some("test/straddle")
                && event.get("id").is_some()
        })
        .expect("the query frame");
    assert!(opened < sent, "the query was written after readiness");
    handle.shutdown().unwrap();
}

#[test]
fn a_held_query_is_bounded_by_the_backstop_and_fails_with_the_connection() {
    needs_python!();
    let mock = Mock::new("held-backstop");
    let gate = Arc::new(AtomicBool::new(false));
    let settings = Settings {
        backstop: TEST_BACKSTOP,
        ..test_settings()
    };
    let handle = mock.spawn(settings, Box::new(Gate(Arc::clone(&gate))));
    initialize(&handle);
    let error = handle
        .query("test/straddle", json!({ "tag": "never" }))
        .unwrap_err()
        .to_string();
    assert!(error.contains("held for readiness"), "{error}");
    assert!(error.contains("inactivity backstop"), "{error}");
    assert_eq!(handle.snapshot().unwrap().pending_operations, 0);
    handle.shutdown().unwrap();
}
