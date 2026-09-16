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

/// A language server that records every launch and then idles.
///
/// The path in `sys.argv[1]` is where each launch appends its pid, which is
/// how the host tests tell one launch from two and check that shutdown left
/// no child behind. It speaks just enough framing for `LspServer::start`:
/// `initialize` is answered with empty capabilities, `exit` ends the process.
pub(crate) const HOST_MOCK_PY: &str = r#"
import json, os, sys, time

log = sys.argv[1] if len(sys.argv) > 1 else None
# Which server this is, so a test can tell two launches apart: process start
# order is the operating system's business, not something to assert on.
label = sys.argv[2] if len(sys.argv) > 2 else "?"
# A fourth argument makes `initialize` take that many seconds to answer, so a
# test can see one launch overlapping another.
stall = float(sys.argv[3]) if len(sys.argv) > 3 else 0.0

def note(what):
    if log:
        with open(log, "a") as handle:
            handle.write("%s %s %d\n" % (what, label, os.getpid()))

note("START")

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
            reply(msg, {"capabilities": {}})
        else:
            reply(msg, None)
    elif "method" in msg:
        if msg["method"] == "exit":
            sys.exit(0)
"#;
