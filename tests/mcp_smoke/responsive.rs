//! MCP control traffic stays answered while a tool call is blocked, calls
//! past the concurrency limit are refused rather than queued, and a blocked
//! call reports progress to a caller that asked for it. Runs
//! everywhere: `rust-analyzer` is a scripted shim on `PATH` that stays
//! not-ready until the test drops a release file, so every ordering below is
//! a barrier, not a timing guess.

use crate::stdio_server::Server;
use serde_json::{Value, json};

/// The fake server: answers `initialize`, reports `quiescent: false`, and
/// only reports `quiescent: true` once the file named by `FAKE_RA_RELEASE`
/// exists. `workspace/symbol` answers empty, so a released `definition` completes
/// with a not-found result.
const FAKE_RUST_ANALYZER: &str = r#"#!/usr/bin/env python3
import json, os, sys, threading, time

lock = threading.Lock()

def send(obj):
    data = json.dumps(obj).encode()
    with lock:
        sys.stdout.buffer.write(b"Content-Length: %d\r\n\r\n" % len(data) + data)
        sys.stdout.buffer.flush()

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

def status(quiescent):
    send({"jsonrpc": "2.0", "method": "experimental/serverStatus",
          "params": {"health": "ok", "quiescent": quiescent}})

def release_watch():
    path = os.environ["FAKE_RA_RELEASE"]
    while not os.path.exists(path):
        time.sleep(0.05)
    status(True)

while True:
    msg = read_msg()
    if msg is None:
        break
    if "method" in msg and "id" in msg:
        method = msg["method"]
        if method == "initialize":
            send({"jsonrpc": "2.0", "id": msg["id"], "result": {
                "capabilities": {},
                "serverInfo": {"name": "fake-rust-analyzer", "version": "0"}}})
            status(False)
            threading.Thread(target=release_watch, daemon=True).start()
        elif method == "workspace/symbol":
            send({"jsonrpc": "2.0", "id": msg["id"], "result": []})
        else:
            send({"jsonrpc": "2.0", "id": msg["id"], "result": None})
    elif msg.get("method") == "exit":
        sys.exit(0)
"#;

/// A scratch dir holding the shim and the release file's path.
struct Shim {
    dir: std::path::PathBuf,
}

impl Shim {
    fn new(tag: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("structural-shim-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let script = dir.join("rust-analyzer");
        std::fs::write(&script, FAKE_RUST_ANALYZER).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        Self { dir }
    }

    fn env(&self) -> Vec<(&'static str, String)> {
        let original = std::env::var("PATH").unwrap_or_default();
        vec![
            ("PATH", format!("{}:{original}", self.dir.display())),
            ("FAKE_RA_RELEASE", self.release().display().to_string()),
        ]
    }

    fn release(&self) -> std::path::PathBuf {
        self.dir.join("release")
    }
}

impl Drop for Shim {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn show(symbol: &str) -> Value {
    json!({ "name": "definition", "arguments": { "symbol": symbol } })
}

#[test]
fn ping_and_overflow_are_answered_while_show_is_blocked() {
    let shim = Shim::new("ping");
    let root = crate::rust_fixture_root();
    let mut server = Server::start_with_env(root.to_str().unwrap(), &shim.env());
    server.request(
        "initialize",
        json!({
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": { "name": "structural-smoke", "version": "0" }
        }),
    );
    server.notify("notifications/initialized", json!({}));

    // Blocked: the fake server never becomes ready until released, so this
    // call sits inside show's readiness wait.
    let blocked = server.send("tools/call", show("blocked"));
    let ping = server.send("ping", json!({}));
    // Sixteen calls may run at once: the blocked one and fifteen more, all
    // waiting on the same server. The seventeenth and later are refused at once.
    let queued: Vec<u64> = (0..15)
        .map(|index| server.send("tools/call", show(&format!("queued{index}"))))
        .collect();
    let refused: Vec<u64> = (0..4)
        .map(|index| server.send("tools/call", show(&format!("refused{index}"))))
        .collect();

    // Everything the reader answers itself arrives first, in arrival order:
    // the pong, then the four refusals. The blocked call is still pending.
    let pong = server.next_response();
    assert_eq!(pong.get("id").and_then(Value::as_u64), Some(ping), "{pong}");
    assert!(pong.get("result").is_some(), "{pong}");
    for id in &refused {
        let response = server.next_response();
        assert_eq!(
            response.get("id").and_then(Value::as_u64),
            Some(*id),
            "{response}"
        );
        assert_eq!(
            response.pointer("/error/code").and_then(Value::as_i64),
            Some(-32000),
            "{response}"
        );
        assert!(
            response
                .pointer("/error/message")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .contains("busy"),
            "{response}"
        );
    }

    // Release: every admitted call completes, each a stated not-found result
    // rather than an error. They run concurrently and take the workspace in
    // turn, so they answer in no particular order.
    std::fs::write(shim.release(), b"go").unwrap();
    let mut expected: std::collections::BTreeSet<u64> =
        std::iter::once(blocked).chain(queued).collect();
    while !expected.is_empty() {
        let response = server.next_response();
        let id = response.get("id").and_then(Value::as_u64);
        assert!(
            id.is_some_and(|id| expected.remove(&id)),
            "unexpected: {response}"
        );
        assert_ne!(
            response.pointer("/result/isError"),
            Some(&json!(true)),
            "{response}"
        );
        let text = response
            .pointer("/result/content/0/text")
            .and_then(Value::as_str)
            .unwrap_or_default();
        assert!(text.starts_with("no symbol named"), "{text}");
    }

    // Still responsive afterwards, and the direct path is untouched.
    let pong = server.request("ping", json!({}));
    assert!(pong.get("result").is_some(), "{pong}");
}

/// A call blocked in a readiness wait tells a caller that sent a progress
/// token that it is alive, and what it is waiting on, before it answers.
#[test]
fn a_blocked_show_reports_progress_to_a_caller_that_asked() {
    let shim = Shim::new("progress");
    let root = crate::rust_fixture_root();
    let mut server = Server::start_with_env(root.to_str().unwrap(), &shim.env());

    let mut call = show("blocked");
    call["_meta"] = json!({ "progressToken": "cold-index" });
    let blocked = server.send("tools/call", call);

    // Ticks name the phase the call is in — launching the server, then
    // waiting for it to index — and count up, as the spec requires.
    let mut last = -1.0;
    loop {
        let progress = server.next_response();
        assert_eq!(
            progress.get("method").and_then(Value::as_str),
            Some("notifications/progress"),
            "{progress}"
        );
        assert_eq!(
            progress.pointer("/params/progressToken"),
            Some(&json!("cold-index"))
        );
        let value = progress
            .pointer("/params/progress")
            .and_then(Value::as_f64)
            .unwrap();
        assert!(value > last, "progress went from {last} to {value}");
        last = value;
        let message = progress
            .pointer("/params/message")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if message.starts_with("waiting for rust-analyzer to finish indexing") {
            break;
        }
        assert!(
            message.starts_with("starting language servers"),
            "{message}"
        );
    }

    std::fs::write(shim.release(), b"go").unwrap();
    let response = server.wait_for(blocked);
    let text = response
        .pointer("/result/content/0/text")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(text.starts_with("no symbol named"), "{response}");
}
