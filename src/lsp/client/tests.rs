use super::*;
use std::io::Cursor;

/// Write `body` as a frame, for feeding a fake stdout.
fn framed(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    write_frame(&mut out, body).unwrap();
    out
}

#[test]
fn incoming_frames_classify_without_a_process() {
    // Feed a reader directly: a response with explicit null classifies as
    // success, a notification passes through, garbage is reported while
    // the stream stays re-synchronizable.
    let stream = framed(br#"{"jsonrpc":"2.0","id":1,"result":null}"#)
        .into_iter()
        .chain(framed(
            br#"{"jsonrpc":"2.0","method":"$/progress","params":{}}"#,
        ))
        .chain(framed(b"not json"))
        .collect::<Vec<u8>>();
    let (tx, rx) = mpsc::channel();
    let handle = thread::spawn(move || reader_loop(BufReader::new(Cursor::new(stream)), tx));
    let first = rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(matches!(first, Incoming::Message(_, true)));
    let second = rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(matches!(second, Incoming::Message(_, false)));
    let third = rx.recv_timeout(Duration::from_secs(1)).unwrap();
    assert!(matches!(third, Incoming::Malformed(_)));
    drop(handle);
}

fn python_available() -> bool {
    Command::new("python3")
        .arg("--version")
        .output()
        .map(|output| output.status.success())
        .unwrap_or(false)
}

fn echo_for(events: &[Value], id: i64) -> Option<Value> {
    events
        .iter()
        .find(|event| {
            event.get("method").and_then(Value::as_str) == Some("test/response-echo")
                && event.pointer("/params/id").and_then(Value::as_i64) == Some(id)
        })
        .cloned()
}

/// Drive the client against a scripted Python server: proves framing,
/// request/response correlation, the *content* of the client's replies to
/// server-initiated requests, unknown-method handling, event collection,
/// and that a late response after a timeout is preserved rather than lost.
/// Skipped when `python3` is unavailable.
#[test]
fn mock_server_round_trip() {
    if !python_available() {
        eprintln!("skipping mock_server_round_trip: python3 not available");
        return;
    }

    let script = mock_script();
    let mut client = Client::spawn("python3", std::slice::from_ref(&script)).unwrap();
    let result = client
        .request(
            "initialize",
            json!({"workspaceFolders": [{"uri": "file:///tmp/ws", "name": "ws"}]}),
            Duration::from_secs(5),
        )
        .unwrap();
    assert!(result.get("capabilities").is_some());

    client.pump(Duration::from_millis(800)).unwrap();

    // The notification and all five server requests were collected.
    assert!(client.events.iter().any(|e| e["method"] == "test/notice"));
    for method in [
        "workspace/configuration",
        "workspace/applyEdit",
        "test/unknown-method",
        "window/workDoneProgress/create",
        "workspace/workspaceFolders",
    ] {
        assert!(
            client.events.iter().any(|e| e["method"] == method),
            "server request {method} was not recorded"
        );
    }

    // ...and the client's *replies* had the right contents.
    let configuration = echo_for(&client.events, 9001).expect("configuration reply");
    assert_eq!(
        configuration.pointer("/params/result"),
        Some(&json!([Value::Null, Value::Null])),
        "workspace/configuration must answer one null per item"
    );

    let apply_edit = echo_for(&client.events, 9002).expect("applyEdit reply");
    assert_eq!(
        apply_edit.pointer("/params/result"),
        Some(&json!({"applied": false})),
        "workspace/applyEdit must be declined, never applied"
    );

    let unknown = echo_for(&client.events, 9003).expect("unknown-method reply");
    assert_eq!(
        unknown
            .pointer("/params/error/code")
            .and_then(Value::as_i64),
        Some(-32601),
        "unknown server requests must get MethodNotFound"
    );

    // The progress handshake reply must carry an explicit result:null,
    // present (not omitted) — this is the reply pyright waits on.
    let progress = echo_for(&client.events, 9004).expect("progress reply");
    assert_eq!(
        progress.pointer("/params/result"),
        Some(&Value::Null),
        "window/workDoneProgress/create must be answered with result:null"
    );
    assert!(
        progress.pointer("/params/method").is_none(),
        "the reply must not carry a method"
    );

    // workspace/workspaceFolders must be answered with the folders offered
    // at initialize, not null or MethodNotFound.
    let folders = echo_for(&client.events, 9005).expect("workspaceFolders reply");
    assert_eq!(
        folders.pointer("/params/result"),
        Some(&json!([{"uri": "file:///tmp/ws", "name": "ws"}])),
        "workspace/workspaceFolders must return the initialized folders"
    );

    // Two outstanding requests, answered in genuine reverse order: the
    // mock holds the slow reply until fast arrives and sends fast FIRST.
    // The client waits for the *slow* one; while pumping, the fast
    // response must be stashed by id — verifiably still in the completed
    // map right after the slow answer returns, before fast is consumed.
    let slow = client.send_request("test/slow", json!({})).unwrap();
    let fast = client.send_request("test/fast", json!({})).unwrap();
    assert_eq!(
        client.wait_response(&slow, Duration::from_secs(5)).unwrap(),
        json!("slow")
    );
    assert!(
        client.completed.contains_key(&fast),
        "the fast response must be stashed in `completed` while waiting on slow, not dropped"
    );
    assert_eq!(
        client.wait_response(&fast, Duration::from_secs(5)).unwrap(),
        json!("fast")
    );
    assert!(client.outstanding.is_empty());
    assert!(client.completed.is_empty());

    // An explicit `"result": null` is success with a null answer, not an
    // error and not a timeout.
    let null_answer = client
        .request("test/nullresult", json!({}), Duration::from_secs(5))
        .unwrap();
    assert_eq!(null_answer, Value::Null);

    // One-shot `request` retires its id on timeout: repeated timeouts
    // with late replies leave both maps empty, and each late reply is
    // preserved as a stray event instead of stranding in the stash.
    // (Contrast the resumable send_request/wait_response pair above.)
    for round in 0..2 {
        let strays_before = client
            .events
            .iter()
            .filter(|e| e.get("result") == Some(&json!("late")))
            .count();
        let error = client
            .request("test/late", json!({}), Duration::from_millis(100))
            .unwrap_err();
        assert!(
            error.to_string().contains("timed out"),
            "round {round}: {error}"
        );
        client.pump(Duration::from_millis(600)).unwrap();
        assert!(
            client.outstanding.is_empty(),
            "round {round}: one-shot timeout must retire the id"
        );
        assert!(
            client.completed.is_empty(),
            "round {round}: a retired id's late reply must not strand in completed"
        );
        let strays_after = client
            .events
            .iter()
            .filter(|e| e.get("result") == Some(&json!("late")))
            .count();
        assert_eq!(
            strays_after,
            strays_before + 1,
            "round {round}: the late reply must be preserved as a stray event"
        );
    }

    // Readiness is protocol state, not the event log: the mock sends one
    // serverStatus notification, and it must be retained even when the
    // event log is capped down to a single entry (which drains older
    // notifications).
    let status = client
        .latest_server_status
        .as_ref()
        .expect("serverStatus retained");
    assert_eq!(status.get("quiescent"), Some(&json!(true)));
    client.set_event_cap(Some(1));
    client.pump(Duration::from_millis(200)).unwrap();
    assert!(client.events.len() <= 1);
    assert!(
        client.latest_server_status.is_some(),
        "capping the event log must not lose the retained readiness state"
    );
    client.set_event_cap(None);

    client.shutdown().unwrap();
    let _ = std::fs::remove_file(&script);
}

/// The retained readiness must follow the LATEST serverStatus —
/// true→false→true — with the event log capped, so no consumer can ever
/// present a stale `quiescent=true` as current.
#[test]
fn latest_server_status_tracks_transitions_under_event_cap() {
    if !python_available() {
        eprintln!("skipping latest_server_status_tracks_transitions: python3 not available");
        return;
    }
    let script = mock_script();
    let mut client = Client::spawn("python3", std::slice::from_ref(&script)).unwrap();

    let set_status = |client: &mut Client, quiescent: bool| {
        client
            .request(
                "test/status",
                json!({ "quiescent": quiescent }),
                Duration::from_secs(5),
            )
            .unwrap();
        client.pump(Duration::from_millis(150)).unwrap();
        client
            .latest_server_status
            .as_ref()
            .expect("serverStatus retained")
            .get("quiescent")
            .cloned()
            .expect("quiescent flag present")
    };

    assert_eq!(set_status(&mut client, true), json!(true));
    // Cap the log: readiness is protocol state, not the capped log.
    client.set_event_cap(Some(1));
    assert_eq!(set_status(&mut client, false), json!(false));
    assert_eq!(set_status(&mut client, true), json!(true));
    assert!(client.events.len() <= 1, "event cap held");
    client.set_event_cap(None);

    client.shutdown().unwrap();
    let _ = std::fs::remove_file(&script);
}

/// A status that arrived while the caller was idle sits in the queue, and
/// the cached `latest_server_status` still shows the previous epoch. The
/// nonblocking drain that `ensure_ready` runs before deciding must
/// process it — otherwise a stale quiescent epoch is presented as
/// current. (`pump` cannot do this: it waits for new arrivals, and a
/// zero timeout receives nothing.)
#[test]
fn drain_queued_processes_a_status_queued_while_idle() {
    if !python_available() {
        eprintln!("skipping drain_queued test: python3 not available");
        return;
    }
    let script = mock_script();
    let mut client = Client::spawn("python3", std::slice::from_ref(&script)).unwrap();

    // Observe quiescent=true.
    client
        .request(
            "test/status",
            json!({ "quiescent": true }),
            Duration::from_secs(5),
        )
        .unwrap();
    client.pump(Duration::from_millis(150)).unwrap();
    assert_eq!(
        client
            .latest_server_status
            .as_ref()
            .unwrap()
            .get("quiescent"),
        Some(&json!(true))
    );

    // Queue a false while idle: the mock acks the request first and
    // emits the status only 150ms later, so it reaches the reader channel
    // after the request returned — nothing dispatches it.
    client
        .request(
            "test/status-later",
            json!({ "quiescent": false }),
            Duration::from_secs(5),
        )
        .unwrap();
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(
        client
            .latest_server_status
            .as_ref()
            .unwrap()
            .get("quiescent"),
        Some(&json!(true)),
        "the false sits queued; only the drain (or a pump) may process it"
    );

    // The drain ensure_ready performs first: the queued false is seen.
    let drained = client.drain_queued().unwrap();
    assert!(drained >= 1, "at least the queued status was drained");
    assert_eq!(
        client
            .latest_server_status
            .as_ref()
            .unwrap()
            .get("quiescent"),
        Some(&json!(false)),
        "the queued false is now the retained status"
    );

    client.shutdown().unwrap();
    let _ = std::fs::remove_file(&script);
}

/// The scripted mock server. Written to the session temp dir by the
/// caller; returns the path.
fn mock_script() -> String {
    let path = std::env::temp_dir().join(format!("structural-mock-lsp-{}.py", std::process::id()));
    std::fs::write(&path, MOCK_PY).unwrap();
    path.to_string_lossy().into_owned()
}

/// A minimal LSP-shaped server: answers initialize, pokes the client with
/// notifications and server-initiated requests whose replies are echoed
/// back, and implements the request/response timing pairs the tests use.
const MOCK_PY: &str = r#"
import json, sys, time

def read_msg():
    length = None
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        if line in (b"\r\n", b"\n"):
            break
        if line.lower().startswith(b"content-length:"):
            length = int(line.split(b":", 1)[1].strip())
    if length is None:
        return None
    return json.loads(sys.stdin.buffer.read(length))

def send(obj):
    data = json.dumps(obj).encode()
    sys.stdout.buffer.write(b"Content-Length: %d\r\n\r\n" % len(data) + data)
    sys.stdout.buffer.flush()

pending_slow = None

while True:
    msg = read_msg()
    if msg is None:
        break
    if "method" in msg and "id" in msg:
        method = msg["method"]
        if method == "initialize":
            send({"jsonrpc": "2.0", "id": msg["id"], "result": {"capabilities": {}}})
            send({"jsonrpc": "2.0", "method": "test/notice", "params": {"n": 1}})
            # Readiness signal the client retains as protocol state.
            send({"jsonrpc": "2.0", "method": "experimental/serverStatus",
                  "params": {"health": "ok", "quiescent": True}})
            # Server-initiated requests the client must answer.
            send({"jsonrpc": "2.0", "id": 9001, "method": "workspace/configuration",
                  "params": {"items": [{}, {}]}})
            send({"jsonrpc": "2.0", "id": 9002, "method": "workspace/applyEdit",
                  "params": {"edit": {"changes": {}}}})
            send({"jsonrpc": "2.0", "id": 9003, "method": "test/unknown-method",
                  "params": {}})
            send({"jsonrpc": "2.0", "id": 9004, "method": "window/workDoneProgress/create",
                  "params": {"token": "t"}})
            send({"jsonrpc": "2.0", "id": 9005, "method": "workspace/workspaceFolders",
                  "params": {}})
        elif method == "test/slow":
            # Hold the slow response: it is only sent when the fast request
            # arrives, so the client's pipe sees fast FIRST, then slow — a
            # genuine reverse-order arrival.
            pending_slow = msg["id"]
        elif method == "test/fast":
            send({"jsonrpc": "2.0", "id": msg["id"], "result": "fast"})
            if pending_slow is not None:
                send({"jsonrpc": "2.0", "id": pending_slow, "result": "slow"})
                pending_slow = None
        elif method == "test/late":
            time.sleep(0.4)
            send({"jsonrpc": "2.0", "id": msg["id"], "result": "late"})
        elif method == "test/status":
            # Emit a serverStatus with the requested quiescent flag, then
            # acknowledge — used to drive readiness transitions.
            send({"jsonrpc": "2.0", "method": "experimental/serverStatus",
                  "params": {"health": "ok", "quiescent": bool(msg["params"]["quiescent"])}})
            send({"jsonrpc": "2.0", "id": msg["id"], "result": "sent"})
        elif method == "test/status-later":
            # Acknowledge FIRST, then emit the serverStatus after a delay: it
            # reaches the client's reader channel while the caller is idle —
            # queued, not dispatched by the request that triggered it.
            send({"jsonrpc": "2.0", "id": msg["id"], "result": "sent"})
            time.sleep(0.15)
            send({"jsonrpc": "2.0", "method": "experimental/serverStatus",
                  "params": {"health": "ok", "quiescent": bool(msg["params"]["quiescent"])}})
        elif method == "test/nullresult":
            send({"jsonrpc": "2.0", "id": msg["id"], "result": None})
        elif method == "shutdown":
            send({"jsonrpc": "2.0", "id": msg["id"], "result": None})
        else:
            send({"jsonrpc": "2.0", "id": msg["id"], "result": "ok"})
    elif "id" in msg:
        # A response from the client: echo it back so the test can assert it.
        send({"jsonrpc": "2.0", "method": "test/response-echo", "params": msg})
"#;
