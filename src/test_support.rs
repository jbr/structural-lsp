//! Scaffolding shared by the crate's unit tests.

/// A Python interpreter that starts quickly. `python3` on PATH may be a
/// version-manager shim that takes over a second to resolve, which
/// would eat the short backstops some tests use; prefer a direct
/// binary, and let `STRUCTURAL_TEST_PYTHON` name one explicitly.
pub(crate) fn python() -> Option<String> {
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

/// Assert that `pid`, a direct child of this process, has been reaped —
/// not merely killed, and not left a zombie.
///
/// `waitpid` with `WNOHANG` tells the three apart: `0` for a live child,
/// the pid for a zombie (which this call then reaps, too late for the
/// assertion), and `ECHILD` once nothing is left to collect. Grandchildren
/// belong to init (or an external subreaper) and can remain zombies until
/// that owner reaps, so this is only for direct children.
#[cfg(unix)]
pub(crate) fn reaped(pid: i32) {
    let mut status = 0;
    assert_eq!(
        unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) },
        -1,
        "child {pid} is still live or unreaped"
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ECHILD)
    );
}

/// A language server that records every launch and then idles.
///
/// The path in `sys.argv[1]` is where each launch appends its pid, which is
/// how the host tests tell one launch from two and check that shutdown left
/// no child behind. It speaks just enough framing for `LspServer::start`:
/// `initialize` is answered with empty capabilities unless a mode below
/// says otherwise, `exit` ends the process.
///
/// `sys.argv[3]` is either a stall in seconds or the word `die`, which
/// records its launch and then exits without answering `initialize`. That
/// is a server that *spawned* and is still unavailable — the distinction
/// the fallback ladder rests on, since availability is successful
/// initialization rather than an executable on `PATH`
/// (`docs/discovery.md`), and it is only observable in the log because the
/// pid is written before the exit.
///
/// Two further modes serve the lifecycle tests. `utf-32` answers
/// `initialize` with a position encoding the client does not convert, so
/// the handshake fails with the child alive and answering. `indexes`
/// advertises the symbol providers, so a launch warms its index
/// (`docs/servers.md`), and records `SYMBOLS` when the warm's
/// `textDocument/documentSymbol` arrives — the last message a launch
/// exchanges with its server before it takes the workspace lock to
/// publish.
pub(crate) const HOST_MOCK_PY: &str = r#"
import json, os, sys, time

log = sys.argv[1] if len(sys.argv) > 1 else None
# Which server this is, so a test can tell two launches apart: process start
# order is the operating system's business, not something to assert on.
label = sys.argv[2] if len(sys.argv) > 2 else "?"
# A fourth argument makes `initialize` take that many seconds to answer, so a
# test can see one launch overlapping another — or, spelled `die`, makes this
# launch record itself and exit without ever answering one. `utf-32` and
# `indexes` change what `initialize` answers.
mode = sys.argv[3] if len(sys.argv) > 3 else ""
try:
    stall = float(mode)
except ValueError:
    stall = 0.0
capabilities = {}
if mode == "utf-32":
    capabilities = {"positionEncoding": "utf-32"}
elif mode == "indexes":
    capabilities = {"workspaceSymbolProvider": True, "documentSymbolProvider": True}

def note(what):
    if log:
        with open(log, "a") as handle:
            handle.write("%s %s %d\n" % (what, label, os.getpid()))

note("START")

if mode == "die":
    sys.exit(1)

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

def send(obj):
    data = json.dumps(obj).encode()
    sys.stdout.buffer.write(b"Content-Length: %d\r\n\r\n" % len(data) + data)
    sys.stdout.buffer.flush()

def reply(msg, result):
    send({"jsonrpc": "2.0", "id": msg["id"], "result": result})

while True:
    msg = read_msg()
    if msg is None:
        break
    if "method" in msg and "id" in msg:
        if msg["method"] == "initialize":
            if stall:
                time.sleep(stall)
            note("READY")
            reply(msg, {"capabilities": capabilities})
        elif msg["method"] == "textDocument/documentSymbol":
            note("SYMBOLS")
            reply(msg, None)
        else:
            reply(msg, None)
    elif "method" in msg:
        if msg["method"] == "exit":
            sys.exit(0)
"#;

/// An operator that keeps every event it is handed, so a test can assert
/// what was reported, how often, and in what order (`docs/operator.md`).
#[derive(Default)]
pub(crate) struct Recorder(std::sync::Mutex<Vec<crate::operator::Event>>);

impl Recorder {
    pub(crate) fn shared() -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self::default())
    }

    pub(crate) fn events(&self) -> Vec<crate::operator::Event> {
        self.0.lock().unwrap().clone()
    }

    /// The events of one kind, in the order they arrived.
    pub(crate) fn of(&self, kind: crate::operator::Kind) -> Vec<crate::operator::Event> {
        self.events()
            .into_iter()
            .filter(|event| event.kind == kind)
            .collect()
    }
}

impl crate::operator::Operator for Recorder {
    fn event(&self, event: &crate::operator::Event) {
        self.0.lock().unwrap().push(event.clone());
    }
}
