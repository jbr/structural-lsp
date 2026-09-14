//! MCP control traffic stays answered while a tool call is blocked, and the
//! tool queue refuses overflow rather than blocking the reader. Runs
//! everywhere: `rust-analyzer` is a scripted shim on `PATH` that stays
//! not-ready until the test drops a release file, so every ordering below is
//! a barrier, not a timing guess.

use crate::stdio_server::Server;
use serde_json::{Value, json};

/// The fake server: answers `initialize`, reports `quiescent: false`, and
/// only reports `quiescent: true` once the file named by `FAKE_RA_RELEASE`
/// exists. `workspace/symbol` answers empty, so a released `show` completes
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
    json!({ "name": "show", "arguments": { "symbol": symbol } })
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
    // The queue holds sixteen; the seventeenth and later are refused at once.
    let queued: Vec<u64> = (0..16)
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

    // Release: the blocked call completes, then the accepted queue drains in
    // arrival order, each a stated not-found result rather than an error.
    std::fs::write(shim.release(), b"go").unwrap();
    for id in std::iter::once(blocked).chain(queued) {
        let response = server.next_response();
        assert_eq!(
            response.get("id").and_then(Value::as_u64),
            Some(id),
            "{response}"
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
        assert!(text.contains("No symbol named"), "{text}");
        assert!(
            text.contains("fake-rust-analyzer 0"),
            "the shim served it: {text}"
        );
    }

    // Still responsive afterwards, and the direct path is untouched.
    let pong = server.request("ping", json!({}));
    assert!(pong.get("result").is_some(), "{pong}");
}
