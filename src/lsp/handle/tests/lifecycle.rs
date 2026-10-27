//! Failure, overload, and teardown.

use super::*;

#[test]
fn eof_fails_every_waiter_and_later_calls() {
    needs_python!();
    let mock = Mock::new("eof");
    let handle = mock.spawn(test_settings(), Box::new(StandardIdle));
    initialize(&handle);
    assert!(handle.notify("test/hello", json!({})).is_ok());

    let pending = thread::spawn({
        let handle = handle.clone_for_test();
        move || handle.request("test/never", json!({}))
    });
    // The mock exits without replying.
    let error = handle.request("test/die", json!({})).unwrap_err();
    assert!(error.to_string().contains("failed"), "{error}");
    let error = pending.join().unwrap().unwrap_err();
    assert!(error.to_string().contains("failed"), "{error}");

    let started = Instant::now();
    let error = handle.request("test/echo", json!({})).unwrap_err();
    assert!(error.to_string().contains("failed"), "{error}");
    assert!(handle.notify("test/hello", json!({})).is_err());
    assert!(handle.wait_ready(Duration::from_secs(5)).is_err());
    assert!(started.elapsed() < Duration::from_secs(1), "fails fast");
    let started = Instant::now();
    handle.shutdown().unwrap();
    assert!(started.elapsed() < Duration::from_secs(3));
}

#[test]
fn shutdown_is_prompt_with_the_writer_genuinely_blocked() {
    needs_python!();
    let mock = Mock::new("blocked");
    let handle = mock.spawn(test_settings(), Box::new(StandardIdle));
    initialize(&handle);
    // The mock answers, then never reads stdin again.
    handle.request("test/stop-reading", json!({})).unwrap();

    // Far more than a pipe buffer: the writer blocks inside the write.
    let big = "x".repeat(600 * 1024);
    let blocked = thread::spawn({
        let handle = handle.clone_for_test();
        move || handle.notify("test/big", json!({ "payload": big }))
    });
    // Let the writer reach the blocked write (a timing fixture for the
    // mock, not a settle wait: shutdown must be correct either way).
    thread::sleep(Duration::from_millis(200));

    let started = Instant::now();
    handle.shutdown().unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "shutdown took {:?}",
        started.elapsed()
    );
    let error = blocked.join().unwrap().unwrap_err();
    assert!(
        error.to_string().contains("not written")
            || error.to_string().contains("failed")
            || error.to_string().contains("closed")
            || error.to_string().contains("shut down")
            || error.to_string().contains("shutting down"),
        "{error}"
    );
}

#[test]
fn required_reply_overflow_terminates_the_connection() {
    needs_python!();
    let mock = Mock::new("storm");
    let settings = Settings {
        writer_cap: 4,
        ..test_settings()
    };
    let handle = mock.spawn(settings, Box::new(StandardIdle));
    initialize(&handle);

    // The mock answers, then reads only the HEADER of our next frame,
    // then emits eight server requests and never reads again. Our next
    // frame is oversized, so the writer is stuck inside its body while
    // the eight required replies queue behind it: the fifth cannot be
    // queued, and that terminates the connection — which is what
    // finally fails this blocked notify.
    assert_eq!(
        handle.request("test/storm", json!({ "n": 8 })).unwrap(),
        json!("stormed")
    );
    let big = "x".repeat(600 * 1024);
    let error = handle
        .notify("test/big", json!({ "payload": big }))
        .unwrap_err();
    assert!(
        error.to_string().contains("not written") || error.to_string().contains("failed"),
        "{error}"
    );
    let error = handle.request("test/never", json!({})).unwrap_err();
    let text = error.to_string();
    assert!(text.contains("could not queue the reply"), "{text}");
    let snapshot = handle.snapshot().unwrap();
    assert!(snapshot.failure.is_some(), "{snapshot:?}");
    let started = Instant::now();
    handle.shutdown().unwrap();
    assert!(started.elapsed() < Duration::from_secs(3));
}

#[test]
fn shutdown_works_under_admission_pressure() {
    needs_python!();
    let mock = Mock::new("saturate");
    let settings = Settings {
        inbound_cap: 2,
        command_cap: 2,
        pending_cap: 2,
        ..test_settings()
    };
    let handle = mock.spawn(settings, Box::new(StandardIdle));
    initialize(&handle);

    let stop = AtomicBool::new(false);
    let first_refusal_for_shutdown = std::sync::Mutex::new(None::<Instant>);
    let flagged_at = thread::scope(|scope| {
        for _ in 0..4 {
            let handle = &handle;
            let stop = &stop;
            let first = &first_refusal_for_shutdown;
            scope.spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    match handle.request("test/never", json!({})) {
                        Ok(_) => panic!("test/never must not be answered"),
                        Err(error) => {
                            let text = error.to_string();
                            if text.contains("shutting down") || text.contains("closed") {
                                first.lock().unwrap().get_or_insert(Instant::now());
                                return;
                            }
                            // Refused for capacity or failed at teardown
                            // are both honest; keep pressing.
                        }
                    }
                }
            });
        }
        thread::sleep(Duration::from_millis(100));
        // The terminate path exactly as `shutdown`/`Drop` take it, minus
        // the join (the scope holds shared borrows).
        let flagged_at = Instant::now();
        handle.terminate();
        thread::sleep(Duration::from_millis(1500));
        stop.store(true, Ordering::SeqCst);
        flagged_at
    });
    let first = first_refusal_for_shutdown
        .lock()
        .unwrap()
        .expect("callers saw the shutdown");
    assert!(
        first.duration_since(flagged_at) < Duration::from_secs(2),
        "owner noticed the flag after {:?}",
        first.duration_since(flagged_at)
    );
    let started = Instant::now();
    handle.shutdown().unwrap();
    assert!(started.elapsed() < Duration::from_secs(3));
}

#[test]
fn a_notification_flood_between_calls_is_consumed_and_bounded() {
    needs_python!();
    let mock = Mock::new("flood");
    let handle = mock.spawn(test_settings(), Box::new(StandardIdle));
    initialize(&handle);

    // Arm: the mock opens a progress token, replies, then — with no request
    // of ours outstanding — emits 10 000 notifications. The token stays open
    // until `test/flood-release` below, so readiness is blocked for a reason
    // this test controls rather than for as long as the owner thread happens
    // to take to drain them. Readiness returns only once the owner has
    // processed the `end`, which the reader delivered after all 10 000 (same
    // producer, FIFO). Nothing pumps; the caller only waits for readiness.

    handle
        .request("test/flood-arm", json!({ "n": 10_000, "token": "flood" }))
        .unwrap();
    // The token is still open — only the release request below ends it — so
    // this says the world is not ready while work is pending, and not that
    // the owner thread happened to be behind.
    assert!(!handle.snapshot().unwrap().ready);
    handle
        .request("test/flood-release", json!({ "token": "flood" }))
        .unwrap();
    handle.wait_ready(Duration::from_secs(20)).unwrap();

    let snapshot = handle.snapshot().unwrap();
    assert!(snapshot.ready);
    assert_eq!(snapshot.pending_operations, 0);
    let events = handle.capture_events().unwrap();
    assert!(events.len() <= 200, "event log capped: {}", events.len());
    let tail: Vec<&Value> = events.iter().rev().take(3).collect();
    assert_eq!(
        events.last().and_then(|e| e.pointer("/params/value/kind")),
        Some(&json!("end")),
        "the last event is the token's end; tail was {tail:?}"
    );
    assert!(
        events.iter().any(|e| e["method"] == "test/noise"),
        "the flood was recorded"
    );
    // The owner is healthy afterwards.
    assert_eq!(handle.request("test/echo", json!({})).unwrap(), json!("ok"));
    handle.shutdown().unwrap();
}

#[test]
fn a_blocked_notification_trips_the_backstop_on_its_own() {
    needs_python!();
    let mock = Mock::new("blocked-notify");
    let settings = Settings {
        backstop: TEST_BACKSTOP,
        ..test_settings()
    };
    let handle = mock.spawn(settings, Box::new(StandardIdle));
    initialize(&handle);
    handle.request("test/stop-reading", json!({})).unwrap();

    // Nothing else is pending: only the notification's own backstop can
    // fail it, and it must — a caller is never left hanging on a server
    // that stopped reading.
    let big = "x".repeat(600 * 1024);
    let started = Instant::now();
    let error = handle
        .notify("test/big", json!({ "payload": big }))
        .unwrap_err();
    assert!(
        error.to_string().contains("inactivity backstop") && error.to_string().contains("test/big"),
        "{error}"
    );
    assert!(
        started.elapsed() < TEST_BACKSTOP * 2,
        "failed after {:?}",
        started.elapsed()
    );
    handle.shutdown().unwrap();
}

#[test]
fn a_full_inbox_does_not_refuse_a_caller() {
    // The inbox a server floods is full; the caller's command queue is
    // not. Admission is decided by the command queue alone, so the caller
    // is admitted however far behind the owner is on the server's frames
    // (docs/transport.md). No owner thread: this is admission itself.
    let (inbox, _inbox_rx) = mpsc::sync_channel(1);
    inbox.try_send(OwnerMsg::Doorbell).unwrap();
    let (commands, commands_rx) = mpsc::sync_channel(1);
    let handle = ServerHandle {
        label: "flooded".to_string(),
        inbox,
        commands,
        command_cap: 1,
        terminate: Arc::new(AtomicBool::new(false)),
        force: Arc::new(AtomicBool::new(false)),
        owner: None,
    };
    let (reply, _) = mpsc::channel();
    handle.admit(Command::Snapshot { reply }).unwrap();
    assert!(matches!(
        commands_rx.try_recv(),
        Ok(Command::Snapshot { .. })
    ));

    // What refuses a caller is its own queue being full.
    let (reply, _) = mpsc::channel();
    handle.admit(Command::Snapshot { reply }).unwrap();
    let (reply, _) = mpsc::channel();
    let error = handle.admit(Command::Snapshot { reply }).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("command queue full (1 commands)"),
        "{error}"
    );
}
