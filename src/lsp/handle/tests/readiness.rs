//! Correlation, readiness, and the inactivity backstop.

use super::*;

#[test]
fn concurrent_requests_correlate_in_reverse_order() {
    needs_python!();
    let mock = Mock::new("pair");
    let handle = mock.spawn(test_settings(), Box::new(StandardIdle));
    initialize(&handle);

    // Whichever `test/pair` arrives first is held until the second one,
    // which is answered first: a genuine reverse-order arrival. Each
    // caller must get its own tag back.
    thread::scope(|scope| {
        for tag in ["a", "b"] {
            let handle = &handle;
            scope.spawn(move || {
                let result = handle.request("test/pair", json!({ "tag": tag })).unwrap();
                assert_eq!(result, json!(tag));
            });
        }
    });

    // An explicit `"result": null` is a null answer, not an error.
    assert_eq!(
        handle.request("test/nullresult", json!({})).unwrap(),
        Value::Null
    );
    // An error response is an Err naming the message.
    let error = handle.request("test/error", json!({})).unwrap_err();
    assert!(error.to_string().contains("boom"), "{error}");

    let snapshot = handle.snapshot().unwrap();
    assert_eq!(snapshot.pending_operations, 0);
    assert!(snapshot.last_message.is_some());
    handle.shutdown().unwrap();
}

#[test]
fn progress_tokens_gate_readiness_and_server_requests_are_answered() {
    needs_python!();
    let mock = Mock::new("progress");
    let handle = mock.spawn(test_settings(), Box::new(StandardIdle));
    initialize(&handle);
    assert!(handle.snapshot().unwrap().ready);
    handle.wait_ready(Duration::from_secs(1)).unwrap();

    // The mock emits the progress frame BEFORE its reply, so when the
    // request returns the owner has already processed it (same
    // producer, FIFO): no timing assumption.
    handle
        .request(
            "test/progress",
            json!({ "action": "create", "token": "t1" }),
        )
        .unwrap();
    let snapshot = handle.snapshot().unwrap();
    assert_eq!(snapshot.open_progress_tokens, 1);
    assert!(!snapshot.ready, "a created token is outstanding work");

    // The create was a server request; the owner must have answered it
    // with an explicit null result (the mock echoes our reply back). Our
    // reply was queued before this echo request, so the mock has read
    // and echoed it by the time it answers here.
    handle.request("test/echo", json!({})).unwrap();
    let events = handle.capture_events().unwrap();
    let echo = echo_for(&events, 7001).expect("create reply echoed");
    assert_eq!(echo.pointer("/params/result"), Some(&Value::Null));

    let waiter = thread::spawn({
        let handle = handle.clone_for_test();
        move || handle.wait_ready(Duration::from_secs(5))
    });
    handle
        .request("test/progress", json!({ "action": "begin", "token": "t2" }))
        .unwrap();
    assert_eq!(handle.snapshot().unwrap().open_progress_tokens, 2);
    handle
        .request("test/progress", json!({ "action": "end", "token": "t1" }))
        .unwrap();
    assert!(!handle.snapshot().unwrap().ready, "t2 still open");
    handle
        .request("test/progress", json!({ "action": "end", "token": "t2" }))
        .unwrap();
    waiter.join().unwrap().unwrap();
    assert!(handle.snapshot().unwrap().ready);
    handle.shutdown().unwrap();
}

/// ccls reserves one `index` token at startup and sends no `$/progress`
/// ever, so counting the create would leave this connection never idle and
/// every readiness wait on it blocking forever
/// (`../../../journal/2026-09-19-ccls-reserves-a-token.md`). A server that
/// says so through `progress_create_is_work = false` is ready after the
/// create, and the progress it *does* report still gates readiness.
#[test]
fn a_reserved_token_a_server_never_reports_through_does_not_gate_readiness() {
    needs_python!();
    let mock = Mock::new("reservation");
    let settings = Settings {
        progress_create_is_work: false,
        ..test_settings()
    };
    let handle = mock.spawn(settings, Box::new(StandardIdle));
    initialize(&handle);

    handle
        .request(
            "test/progress",
            json!({ "action": "create", "token": "index" }),
        )
        .unwrap();
    let snapshot = handle.snapshot().unwrap();
    assert_eq!(snapshot.open_progress_tokens, 0);
    assert!(snapshot.ready, "a reservation is not work for this server");
    handle.wait_ready(Duration::from_secs(1)).unwrap();

    // What the server does report is tracked as it always was.
    handle
        .request(
            "test/progress",
            json!({ "action": "begin", "token": "index" }),
        )
        .unwrap();
    assert!(!handle.snapshot().unwrap().ready, "begin is work");
    handle
        .request(
            "test/progress",
            json!({ "action": "end", "token": "index" }),
        )
        .unwrap();
    assert!(handle.snapshot().unwrap().ready);
    handle.shutdown().unwrap();
}

#[test]
fn policy_composes_with_standard_progress() {
    needs_python!();
    let mock = Mock::new("policy");
    let gate = Arc::new(AtomicBool::new(false));
    let handle = mock.spawn(test_settings(), Box::new(Gate(Arc::clone(&gate))));
    initialize(&handle);

    // No tokens, but the policy says not ready: not ready.
    assert!(!handle.snapshot().unwrap().ready);
    let waiter = thread::spawn({
        let handle = handle.clone_for_test();
        move || handle.wait_ready(Duration::from_secs(5))
    });
    handle
        .request("test/gate", json!({ "open": true }))
        .unwrap();
    waiter.join().unwrap().unwrap();
    assert!(handle.snapshot().unwrap().ready);

    // Policy ready, but a token open: the policy cannot override
    // standard progress.
    handle
        .request("test/progress", json!({ "action": "begin", "token": "x" }))
        .unwrap();
    assert!(!handle.snapshot().unwrap().ready);
    handle
        .request("test/progress", json!({ "action": "end", "token": "x" }))
        .unwrap();
    assert!(handle.snapshot().unwrap().ready);
    handle.shutdown().unwrap();
}

#[test]
fn silence_trips_the_backstop_and_the_connection_survives() {
    needs_python!();
    let mock = Mock::new("silence");
    let settings = Settings {
        backstop: TEST_BACKSTOP,
        ..test_settings()
    };
    let handle = mock.spawn(settings, Box::new(StandardIdle));
    initialize(&handle);

    let started = Instant::now();
    let error = handle.request("test/never", json!({})).unwrap_err();
    let elapsed = started.elapsed();
    let text = error.to_string();
    assert!(text.contains("python"), "names the server: {text}");
    assert!(text.contains("inactivity backstop"), "{text}");
    assert!(text.contains("last message: response"), "{text}");
    assert!(
        elapsed >= TEST_BACKSTOP && elapsed < TEST_BACKSTOP * 2,
        "tripped after {elapsed:?}"
    );

    // The server is silent, not dead: the connection stays usable.
    assert_eq!(handle.request("test/echo", json!({})).unwrap(), json!("ok"));
    handle.shutdown().unwrap();
}

#[test]
fn activity_extends_the_backstop() {
    needs_python!();
    let mock = Mock::new("chatter");
    let settings = Settings {
        backstop: TEST_BACKSTOP,
        ..test_settings()
    };
    let handle = mock.spawn(settings, Box::new(StandardIdle));
    initialize(&handle);

    // Six notifications 700 ms apart, then the reply: 4.2 s of a 3 s
    // backstop, never silent for more than 700 ms. The period scales with
    // the backstop — shrink it and the chatter stops extending anything,
    // which is the property this test is about.
    let result = handle
        .request("test/chatter", json!({ "count": 6, "period_ms": 700 }))
        .unwrap();
    assert_eq!(result, json!("done"));
    handle.shutdown().unwrap();
}

#[test]
fn a_long_idle_connection_accepts_a_new_request() {
    needs_python!();
    let mock = Mock::new("idle");
    let settings = Settings {
        backstop: TEST_BACKSTOP,
        ..test_settings()
    };
    let handle = mock.spawn(settings, Box::new(StandardIdle));
    initialize(&handle);
    // Idle for longer than the backstop: the next operation starts its
    // own clock at admission.
    thread::sleep(TEST_BACKSTOP + Duration::from_secs(1));
    assert_eq!(handle.request("test/echo", json!({})).unwrap(), json!("ok"));
    handle.shutdown().unwrap();
}

#[test]
fn a_quiet_server_that_answers_probes_is_waited_for() {
    needs_python!();
    let mock = Mock::new("quiet");
    let settings = Settings {
        backstop: TEST_BACKSTOP,
        probe_after: Duration::from_millis(300),
        ..test_settings()
    };
    let handle = mock.spawn(settings, Box::new(StandardIdle));
    initialize(&handle);
    // Longer than the backstop of silence inside an open token: only the
    // probes keep the wait alive, and they must.
    handle
        .request("test/quiet-work", json!({ "secs": 4 }))
        .unwrap();
    let started = Instant::now();
    handle.wait_ready(Duration::from_secs(1)).unwrap();
    assert!(
        started.elapsed() >= TEST_BACKSTOP,
        "{:?}",
        started.elapsed()
    );
    let events = handle.capture_events().unwrap();
    let probes = events
        .iter()
        .filter(|e| e["method"] == crate::lsp::handle::PROBE_METHOD)
        .count();
    assert!(probes >= 3, "probes sent during the silence: {probes}");
    handle.shutdown().unwrap();
}

#[test]
fn a_deaf_server_trips_the_backstop_with_the_probe_named() {
    needs_python!();
    let mock = Mock::new("deaf");
    let settings = Settings {
        backstop: TEST_BACKSTOP,
        probe_after: Duration::from_millis(300),
        ..test_settings()
    };
    let handle = mock.spawn(settings, Box::new(StandardIdle));
    initialize(&handle);
    handle.request("test/deaf-work", json!({})).unwrap();
    let started = Instant::now();
    let error = handle.wait_ready(Duration::from_secs(1)).unwrap_err();
    let text = error.to_string();
    assert!(text.contains("inactivity backstop"), "{text}");
    assert!(text.contains("liveness probe went unanswered"), "{text}");
    assert!(
        started.elapsed() < TEST_BACKSTOP * 2,
        "{:?}",
        started.elapsed()
    );
    handle.shutdown().unwrap();
}
