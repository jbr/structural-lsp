//! Lifecycle integration tests use mock request and signal-handler events to
//! choose interleavings. All reads and child cleanup are bounded by the fixture.
#![cfg(unix)]

use crate::stdio_server::Server;
use serde_json::{Value, json};

const MOCK: &str = r#"
import json, os, sys, threading

def event(text):
    print('mock: ' + text, file=sys.stderr, flush=True)

def send(obj):
    data = json.dumps(obj).encode()
    sys.stdout.buffer.write(b'Content-Length: %d\r\n\r\n' % len(data) + data)
    sys.stdout.buffer.flush()

def reply(msg, result):
    send({'jsonrpc': '2.0', 'id': msg['id'], 'result': result})

def read_msg():
    length = None
    while True:
        line = sys.stdin.buffer.readline()
        if not line: return None
        if line in (b'\r\n', b'\n'): break
        if line.lower().startswith(b'content-length:'):
            length = int(line.split(b':', 1)[1])
    return json.loads(sys.stdin.buffer.read(length))

event('started ' + str(os.getpid()))
while True:
    msg = read_msg()
    if msg is None: break
    method = msg.get('method')
    if method == 'initialize':
        if os.environ.get('HOLD_INIT'):
            event('initialization pending')
        else:
            # The stuck query below is a workspace/symbol this mock never
            # answers, so it has to advertise the capability: structural
            # does not send a request a server did not offer.
            reply(msg, {'capabilities': {'workspaceSymbolProvider': True}})
    elif method == 'initialized':
        send({'jsonrpc': '2.0', 'method': 'experimental/serverStatus', 'params': {'quiescent': True}})
    elif method == 'workspace/symbol':
        event('query pending')
        if os.environ.get('RELEASE_FIFO'):
            # Test opens this only after the first signal was handled.
            with open(os.environ['RELEASE_FIFO']) as release:
                release.read(1)
            event('query released')
            reply(msg, [])
    elif method == 'shutdown':
        event('shutdown')
        reply(msg, None)
    elif method == 'exit':
        sys.exit(0)
    elif 'id' in msg:
        send({'jsonrpc': '2.0', 'id': msg['id'], 'error': {'code': -32601, 'message': 'MethodNotFound'}})
"#;

struct Shim {
    dir: std::path::PathBuf,
}

impl Shim {
    fn new(tag: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("structural-shutdown-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let python = [
            "/opt/homebrew/bin/python3",
            "/usr/bin/python3",
            "/usr/local/bin/python3",
        ]
        .into_iter()
        .find(|p| std::path::Path::new(p).exists())
        .expect("Python 3 for mock");
        let script = dir.join("rust-analyzer");
        std::fs::write(&script, format!("#!{python}\n{MOCK}")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(script, std::fs::Permissions::from_mode(0o755)).unwrap();
        Self { dir }
    }

    fn env(&self) -> Vec<(&'static str, String)> {
        vec![("PATH", self.dir.display().to_string())]
    }

    fn spawn(&self) -> Server {
        Server::start_with_env(crate::rust_fixture_root().to_str().unwrap(), &self.env())
    }
}

impl Drop for Shim {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn initialize(server: &mut Server) {
    let result = server.request(
        "initialize",
        json!({
            "protocolVersion": "2025-11-25", "capabilities": {},
            "clientInfo": {"name": "shutdown-smoke", "version": "0"}
        }),
    );
    assert!(result.get("result").is_some(), "{result}");
}

fn started(server: &Server) -> i32 {
    server
        .event("mock: started ")
        .split_whitespace()
        .last()
        .unwrap()
        .parse()
        .unwrap()
}

fn query(server: &mut Server) -> u64 {
    let id = server.send(
        "tools/call",
        json!({"name": "definition", "arguments": {"symbol": "warmup"}}),
    );
    server.event("mock: query pending");
    id
}

fn signal(server: &Server) {
    // The test still owns an unreaped child; no cached PID survives reap.
    assert_eq!(
        unsafe { libc::kill(server.child.id() as i32, libc::SIGTERM) },
        0
    );
}

fn dead(pid: i32) {
    assert_eq!(
        unsafe { libc::kill(pid, 0) },
        -1,
        "mock {pid} survived frontend cleanup"
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
}

fn failed(value: &Value) {
    assert!(
        value.get("error").is_some() || value.pointer("/result/isError") == Some(&json!(true)),
        "stuck request must fail, not return a successful partial answer: {value}"
    );
}

#[test]
fn closing_stdin_reaps_a_stuck_server() {
    let shim = Shim::new("eof");
    let mut server = shim.spawn();
    let pid = started(&server);
    initialize(&mut server);
    let blocked = query(&mut server);
    server.close_stdin();
    // Keep stdout drained: orderly input EOF is not output pipe loss.
    failed(&server.wait_for(blocked));
    assert!(server.wait_exit().success());
    dead(pid);
}

#[test]
fn second_signal_interrupts_stuck_query() {
    let shim = Shim::new("force");
    let mut server = shim.spawn();
    let pid = started(&server);
    initialize(&mut server);
    let blocked = query(&mut server);
    signal(&server);
    server.event("structural: graceful shutdown requested");
    // No response wait here: mcplease queues tool calls behind the stuck one.
    assert!(server.child.try_wait().unwrap().is_none());
    signal(&server);
    server.event("structural: forced shutdown requested");
    failed(&server.wait_for(blocked));
    assert!(server.wait_exit().success());
    dead(pid);
}

#[test]
fn first_signal_finishes_idle_serve_with_stdin_open() {
    let shim = Shim::new("idle");
    let mut server = shim.spawn();
    let pid = started(&server);
    initialize(&mut server);
    signal(&server);
    server.event("structural: graceful shutdown requested");
    server.event("mock: shutdown");
    assert!(server.wait_exit().success());
    dead(pid);
}

#[test]
fn signals_are_installed_before_initialization() {
    let shim = Shim::new("init");
    let mut env = shim.env();
    env.push(("HOLD_INIT", "1".into()));
    let mut server = Server::start_with_env(crate::rust_fixture_root().to_str().unwrap(), &env);
    let pid = started(&server);
    server.event("mock: initialization pending");
    signal(&server);
    server.event("structural: graceful shutdown requested");
    signal(&server);
    server.event("structural: forced shutdown requested");
    assert!(server.wait_exit().success());
    dead(pid);
}

#[test]
fn cli_help_exits_without_stdin_eof() {
    let shim = Shim::new("cli");
    let mut server = Server::start_args(
        crate::rust_fixture_root().to_str().unwrap(),
        &shim.env(),
        &["--help"],
    );
    let pid = started(&server);
    assert!(server.wait_exit().success());
    dead(pid);
}

#[test]
fn first_signal_allows_admitted_query_and_drains_response() {
    use std::io::Write;
    use std::os::unix::ffi::OsStrExt;
    let shim = Shim::new("soft-call");
    let path = shim.dir.join("release");
    let cpath = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(cpath.as_ptr(), 0o600) }, 0);
    // Keep both ends open in the driver, so creating the release handle
    // cannot block waiting for the mock to reach its query barrier.
    let mut release = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&path)
        .unwrap();
    let mut env = shim.env();
    env.push(("RELEASE_FIFO", path.display().to_string()));
    let mut server = Server::start_with_env(crate::rust_fixture_root().to_str().unwrap(), &env);
    let pid = started(&server);
    initialize(&mut server);
    let blocked = query(&mut server);
    signal(&server);
    server.event("structural: graceful shutdown requested");
    assert!(server.child.try_wait().unwrap().is_none());
    release.write_all(b"x").unwrap();
    server.event("mock: query released");
    let response = server.wait_for(blocked);
    let text = response.to_string();
    assert!(
        !text.contains("shutdown") && !text.contains("shutting down"),
        "{response}"
    );
    server.event("mock: shutdown");
    assert!(server.wait_exit().success());
    dead(pid);
}
