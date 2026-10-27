//! Two servers offer the same name, and one of them answers the symbol
//! index while it is not loaded, or cut at its cap: the answer must say so
//! rather than resolve to the other server's candidate alone
//! (docs/correctness.md, docs/resolution.md).
//!
//! Both built-ins are overridden to run a scripted mock, so the Rust one
//! keeps rust-analyzer's adapter — its quiescence policy, its `null`
//! reading and its symbol cap — while answering what each case scripts.
//! Ordering comes from the mock's one output stream, never from timing.

use serde_json::json;
use std::path::PathBuf;

/// `argv`: the label (`rust` or `python`) and a mode.
///
/// The Rust side reports quiescence the way rust-analyzer does; in `race`
/// the first `workspace/symbol` is answered the way rust-analyzer answers
/// one that lands as it begins a workspace reload — `quiescent: false`
/// after the request was read, then `null`, then `quiescent: true`. In
/// `null` every `workspace/symbol` is `null` with no status change, and in
/// `capped` it is as many near-misses as the limit structural set at
/// `initialize`. `absent` answers with nothing at all.
const MOCK_PY: &str = r#"
import json, sys

label = sys.argv[1]
mode = sys.argv[2]

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
    return json.loads(sys.stdin.buffer.read(length))

def send(obj):
    data = json.dumps(obj).encode()
    sys.stdout.buffer.write(b"Content-Length: %d\r\n\r\n" % len(data) + data)
    sys.stdout.buffer.flush()

def reply(msg, result):
    send({"jsonrpc": "2.0", "id": msg["id"], "result": result})

def status(quiescent):
    send({"jsonrpc": "2.0", "method": "experimental/serverStatus",
          "params": {"health": "ok", "quiescent": quiescent}})

def rng(line, start, end):
    return {"start": {"line": line, "character": start},
            "end": {"line": line, "character": end}}

# Where `tool_call` is named in each fixture file.
FILES = {"rust": ("src/lib.rs", rng(0, 7, 16)), "python": ("app/tool.py", rng(0, 4, 13))}
root = None
limit = None
raced = False

def symbol(name, line_range):
    path, _ = FILES[label]
    return {"name": name, "kind": 12,
            "location": {"uri": root + "/" + path, "range": line_range}}

while True:
    msg = read_msg()
    if msg is None:
        break
    method = msg.get("method")
    if method and "id" in msg:
        params = msg.get("params") or {}
        if method == "initialize":
            root = params["rootUri"]
            limit = (params.get("initializationOptions") or {}).get(
                "workspace", {}).get("symbol", {}).get("search", {}).get("limit")
            reply(msg, {"capabilities": {"workspaceSymbolProvider": True,
                                         "definitionProvider": True}})
        elif method == "workspace/symbol":
            exact = [symbol("tool_call", FILES[label][1])]
            if mode == "race" and not raced and params.get("query") == "tool_call":
                raced = True
                status(False)
                reply(msg, None)
                status(True)
            elif mode == "null":
                reply(msg, None)
            elif mode == "capped":
                reply(msg, [symbol("a_tool_call_%04d" % i, rng(0, 0, 1)) for i in range(limit)])
            elif mode == "capped-tail":
                reply(msg, [symbol("a_tool_call_%04d" % i, rng(0, 0, 1))
                            for i in range(limit - 1)] + exact)
            elif mode == "absent":
                reply(msg, [])
            else:
                reply(msg, exact)
        elif method == "textDocument/definition":
            uri = params["textDocument"]["uri"]
            reply(msg, {"uri": uri, "range": FILES[label][1]})
        else:
            reply(msg, None)
    elif method == "initialized" and label == "rust":
        status(True)
    elif method == "exit":
        sys.exit(0)
"#;

/// A tree with `tool_call` in Rust and in Python, both servers mocked.
fn scratch(name: &str, rust: &str, python: &str) -> PathBuf {
    let base = std::env::temp_dir().join(format!(
        "structural-readiness-race-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&base);
    let root = base.join("root");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(root.join("app")).unwrap();
    std::fs::write(root.join("Cargo.toml"), "").unwrap();
    std::fs::write(root.join("src/lib.rs"), "pub fn tool_call() {}\n").unwrap();
    std::fs::write(root.join("app/tool.py"), "def tool_call():\n    pass\n").unwrap();
    // Outside the root: a `.py` file inside it would be read as evidence.
    let mock = base.join("mock.py");
    std::fs::write(&mock, MOCK_PY).unwrap();
    let python_bin = super::mock_scope::python();
    let command = |label: &str, mode: &str| {
        format!(
            "command = [\"{python_bin}\", \"{}\", \"{label}\", \"{mode}\"]",
            mock.display()
        )
    };
    let mut config = format!(
        "[servers.rust-analyzer]\n{}\n\n[servers.pyright]\n{}\n\n",
        command("rust", rust),
        command("python", python)
    );
    // Out of consideration, so the machine's own servers for these
    // languages are never selected beside the mocks (docs/catalogue.md).
    for other in [
        "basedpyright",
        "jedi",
        "pyrefly",
        "zuban",
        "ty",
        "pylsp",
        "pylyzer",
        "ruff",
        "ruff-lsp",
    ] {
        config.push_str(&format!("[servers.{other}]\ndisabled = true\n\n"));
    }
    std::fs::write(root.join("structural.toml"), config).unwrap();
    std::fs::canonicalize(root).unwrap()
}

fn definition(root: &std::path::Path, symbol: &str) -> anyhow::Result<String> {
    let state = structural_lsp::session::Session::new(root.to_path_buf());
    let tool: structural_lsp::tools::Definition =
        serde_json::from_value(json!({ "symbol": symbol })).unwrap();
    tool.run(&state)
}

#[test]
fn an_index_answer_produced_as_work_began_is_asked_again_not_read_as_empty() {
    let root = scratch("race", "race", "plain");
    let answer = definition(&root, "tool_call").unwrap_or_else(|error| panic!("{error:#}"));
    assert!(
        answer.starts_with("ambiguous: 2 definitions of 'tool_call'\n"),
        "both servers' candidates, not the Python one alone: {answer}"
    );
    assert!(answer.contains("src/lib.rs:1"), "{answer}");
    assert!(answer.contains("app/tool.py:1"), "{answer}");
}

#[test]
fn a_null_index_answer_with_no_work_reported_is_an_error_naming_the_server() {
    let root = scratch("null", "null", "plain");
    let error = definition(&root, "tool_call").unwrap_err();
    let message = format!("{error:#}");
    assert!(
        message.contains("rust-analyzer answered workspace/symbol with null"),
        "{message}"
    );
    assert!(message.contains("not loaded"), "{message}");
}

#[test]
fn an_index_answer_cut_at_its_cap_is_never_a_silent_miss_or_a_silent_pick() {
    // Beside another server's exact match: one definition is listed, and
    // not resolved.
    let root = scratch("capped-beside", "capped", "plain");
    let answer = definition(&root, "tool_call").unwrap_or_else(|error| panic!("{error:#}"));
    assert!(
        answer
            .starts_with("one definition of 'tool_call' found, and it may not be the only one:\n"),
        "{answer}"
    );
    assert!(answer.contains("app/tool.py:1"), "{answer}");
    assert!(
        answer.contains("note: rust-analyzer's symbol index answered with its cap of 1024 matches"),
        "{answer}"
    );

    // Alone: the miss is within what the index returned, and says so.
    let root = scratch("capped-alone", "capped", "absent");
    let answer = definition(&root, "tool_call").unwrap_or_else(|error| panic!("{error:#}"));
    assert!(
        answer.starts_with(
            "no exact match for 'tool_call' among the matches the symbol index returned; similar:\n"
        ),
        "{answer}"
    );
    assert!(
        answer.contains("a `path` naming the declaring file resolves from that file's outline"),
        "{answer}"
    );

    // An answer at its cap that ends in the name: its exact match is
    // nominated, and more of the same name may follow, so the note stands.
    let root = scratch("capped-tail", "capped-tail", "absent");
    let answer = definition(&root, "tool_call").unwrap_or_else(|error| panic!("{error:#}"));
    assert!(
        answer
            .starts_with("one definition of 'tool_call' found, and it may not be the only one:\n"),
        "{answer}"
    );
    assert!(answer.contains("src/lib.rs:1"), "{answer}");
    assert!(
        answer.contains("matches, ending in the name, so more definitions of it may be past"),
        "{answer}"
    );
}
