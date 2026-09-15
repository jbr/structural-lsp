//! Shared fixtures for the handle tests: a fast interpreter, the scripted
//! mock server, a gate policy, and a second caller-side handle.

use super::*;
use std::sync::atomic::AtomicBool;

/// A Python interpreter that starts quickly. `python3` on PATH may be a
/// version-manager shim that takes over a second to resolve, which
/// would eat the short backstops some tests use; prefer a direct
/// binary, and let `STRUCTURAL_TEST_PYTHON` name one explicitly.
fn python() -> Option<String> {
    let explicit = std::env::var("STRUCTURAL_TEST_PYTHON").ok();
    let candidates = [
        "/opt/homebrew/bin/python3",
        "/usr/local/bin/python3",
        "/usr/bin/python3",
        "python3",
    ];
    explicit
        .iter()
        .map(String::as_str)
        .chain(candidates)
        .find(|candidate| {
            std::process::Command::new(candidate)
                .arg("--version")
                .output()
                .map(|output| output.status.success())
                .unwrap_or(false)
        })
        .map(str::to_string)
}

/// Small capacities so overload paths are reachable; a long backstop so
/// only the tests that set their own can trip it.
fn test_settings() -> Settings {
    Settings {
        probe_after: Duration::from_secs(60),
        inbound_cap: 64,
        writer_cap: 8,
        pending_cap: 8,
        progress_token_cap: 8,
        event_cap: 200,
        backstop: Duration::from_secs(5),
        shutdown_grace: Duration::from_millis(500),
    }
}

struct Mock {
    python: String,
    script: std::path::PathBuf,
}

impl Mock {
    fn new(name: &str) -> Self {
        let script = std::env::temp_dir().join(format!(
            "structural-handle-mock-{name}-{}.py",
            std::process::id()
        ));
        std::fs::write(&script, MOCK_PY).unwrap();
        Self {
            python: python().expect("checked by needs_python!"),
            script,
        }
    }

    fn spawn(&self, settings: Settings, policy: Box<dyn ReadinessPolicy>) -> ServerHandle {
        let args = vec![self.script.to_string_lossy().into_owned()];
        ServerHandle::spawn_with(&self.python, &args, settings, policy).unwrap()
    }
}

impl Drop for Mock {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.script);
    }
}

/// A readiness policy driven by `test/gate` notifications.
struct Gate(Arc<AtomicBool>);

impl ReadinessPolicy for Gate {
    fn observe(&mut self, method: &str, params: Option<&Value>) {
        if method == "test/gate" {
            let open = params
                .and_then(|p| p.get("open"))
                .and_then(Value::as_bool)
                .unwrap_or(false);
            self.0.store(open, Ordering::SeqCst);
        }
    }
    fn is_ready(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

fn initialize(handle: &ServerHandle) {
    let result = handle
        .request(
            "initialize",
            json!({"workspaceFolders": [{"uri": "file:///tmp/ws", "name": "ws"}]}),
        )
        .unwrap();
    assert!(result.get("capabilities").is_some());
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

macro_rules! needs_python {
    () => {
        if python().is_none() {
            eprintln!("skipping: python3 not available");
            return;
        }
    };
}

mod lifecycle;
mod readiness;

impl ServerHandle {
    /// A second caller-side handle onto the same connection, for tests
    /// that need a call outstanding on another thread while this one
    /// keeps driving. It never owns the owner thread, so its `Drop` is
    /// a no-op; teardown stays with the original.
    fn clone_for_test(&self) -> ServerHandle {
        ServerHandle {
            label: self.label.clone(),
            inbox: self.inbox.clone(),
            inbound_cap: self.inbound_cap,
            terminate: Arc::clone(&self.terminate),
            owner: None,
            join_bound: self.join_bound,
        }
    }
}

/// The scripted server behind every test above.
const MOCK_PY: &str = r#"
import json, sys, threading, time

def read_headers():
    length = None
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        if line in (b"\r\n", b"\n"):
            break
        if line.lower().startswith(b"content-length:"):
            length = int(line.split(b":", 1)[1].strip())
    return length

def read_msg():
    length = read_headers()
    if length is None:
        return None
    return json.loads(sys.stdin.buffer.read(length))

lock = threading.Lock()

def send(obj):
    data = json.dumps(obj).encode()
    with lock:
        sys.stdout.buffer.write(b"Content-Length: %d\r\n\r\n" % len(data) + data)
        sys.stdout.buffer.flush()

def reply(msg, result):
    send({"jsonrpc": "2.0", "id": msg["id"], "result": result})

def notify(method, params):
    send({"jsonrpc": "2.0", "method": method, "params": params})

def progress(token, kind):
    notify("$/progress", {"token": token, "value": {"kind": kind}})

held_pair = None
next_server_id = 7001

while True:
    msg = read_msg()
    if msg is None:
        break
    if "method" in msg and "id" in msg:
        method = msg["method"]
        params = msg.get("params") or {}
        if method == "initialize":
            reply(msg, {"capabilities": {}})
        elif method == "test/pair":
            if held_pair is None:
                held_pair = msg
            else:
                reply(msg, params["tag"])
                reply(held_pair, held_pair["params"]["tag"])
                held_pair = None
        elif method == "test/nullresult":
            reply(msg, None)
        elif method == "test/error":
            send({"jsonrpc": "2.0", "id": msg["id"],
                  "error": {"code": -32000, "message": "boom"}})
        elif method == "test/never":
            pass
        elif method == "test/die":
            sys.exit(0)
        elif method == "test/chatter":
            for i in range(int(params["count"])):
                time.sleep(params["period_ms"] / 1000.0)
                notify("test/tick", {"i": i})
            reply(msg, "done")
        elif method == "test/progress":
            token = params["token"]
            action = params["action"]
            if action == "create":
                send({"jsonrpc": "2.0", "id": next_server_id,
                      "method": "window/workDoneProgress/create",
                      "params": {"token": token}})
                next_server_id += 1
            else:
                progress(token, action)
            reply(msg, "sent")
        elif method == "test/gate":
            notify("test/gate", {"open": bool(params["open"])})
            reply(msg, "sent")
        elif method == "test/flood-arm":
            token = params["token"]
            progress(token, "begin")
            reply(msg, "armed")
            for i in range(int(params["n"])):
                notify("test/noise", {"i": i})
            progress(token, "end")
        elif method == "test/quiet-work":
            # Work in progress and nothing said for a while, but the loop
            # keeps answering requests (a probe gets the default reply).
            progress("quiet", "begin")
            reply(msg, "working")
            def finish(secs):
                time.sleep(secs)
                progress("quiet", "end")
            threading.Thread(target=finish, args=(params["secs"],), daemon=True).start()
        elif method == "test/deaf-work":
            # Work in progress, then the loop stops answering anything.
            progress("deaf", "begin")
            reply(msg, "working")
            while True:
                time.sleep(3600)
        elif method == "test/stop-reading":
            reply(msg, "stopped")
            while True:
                time.sleep(3600)
        elif method == "test/storm":
            reply(msg, "stormed")
            # Observe the client's next frame arriving (its header only),
            # then demand replies without ever reading again.
            read_headers()
            for i in range(int(params["n"])):
                send({"jsonrpc": "2.0", "id": 8000 + i, "method": "test/ping-me",
                      "params": {}})
            while True:
                time.sleep(3600)
        elif method == "shutdown":
            reply(msg, None)
        else:
            reply(msg, "ok")
    elif "method" in msg:
        if msg["method"] == "exit":
            sys.exit(0)
    elif "id" in msg:
        # A response from the client: echo it back so tests can assert it.
        notify("test/response-echo", msg)
"#;
