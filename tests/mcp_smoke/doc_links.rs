//! The rename caveat and doc-link edits, deterministically: a mock running
//! under the rust-analyzer adapter (built-in name, overridden command) whose
//! rename answer includes an intra-doc-link edit, and a wholly generic mock
//! whose answer does not. No real server here — the caveat is the adapter's
//! policy, so it must ride on both a plan that edits doc links and one that
//! does not, and a returned doc-link edit is applied faithfully like any
//! other.

use std::path::PathBuf;
use std::process::Command;

/// The scratch file the mocks rename in: an intra-doc link to `greet` in a
/// doc comment, a call site, and the definition.
const DOC_RS: &str = "\
/// Links [`greet`] for the docs.
pub fn doc_me() -> usize {
    greet()
}

pub fn greet() -> usize {
    1
}
";

/// The JSON-RPC plumbing both mocks share: framed read and write, and a
/// range shorthand. Each mock appends its own `while` dispatch loop.
const PREAMBLE: &str = r#"
import json, sys

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

def rng(sl, sc, el, ec):
    return {"start": {"line": sl, "character": sc}, "end": {"line": el, "character": ec}}
"#;

/// A server under the rust-analyzer adapter: quiescent on demand, and its
/// rename rewrites the doc link, the call site and the definition.
const RA_MOCK_BODY: &str = r#"
opened = []

while True:
    msg = read_msg()
    if msg is None:
        break
    if "method" in msg and "id" in msg:
        method = msg["method"]
        if method == "initialize":
            reply(msg, {"capabilities": {
                "documentSymbolProvider": True,
                "workspaceSymbolProvider": True,
                "renameProvider": True}})
        elif method == "workspace/symbol":
            reply(msg, [{"name": "greet", "kind": 12,
                         "location": {"uri": opened[0] if opened else "",
                                      "range": rng(5, 7, 5, 12)}}])
        elif method == "textDocument/definition":
            reply(msg, {"uri": msg["params"]["textDocument"]["uri"],
                        "range": rng(5, 7, 5, 12)})
        elif method == "textDocument/documentSymbol":
            reply(msg, [{"name": "greet", "kind": 12, "range": rng(5, 0, 6, 1),
                         "selectionRange": rng(5, 7, 5, 12),
                         "detail": None, "children": []}])
        elif method == "textDocument/rename":
            uri = msg["params"]["textDocument"]["uri"]
            reply(msg, {"changes": {uri: [
                {"range": rng(0, 12, 0, 17), "newText": "salute"},
                {"range": rng(2, 4, 2, 9), "newText": "salute"},
                {"range": rng(5, 7, 5, 12), "newText": "salute"}]}})
        else:
            reply(msg, None)
    elif msg.get("method") == "exit":
        sys.exit(0)
    else:
        if msg.get("method") == "textDocument/didOpen":
            opened.append(msg["params"]["textDocument"]["uri"])
        # The rust-analyzer adapter waits for this to call the server ready.
        send({"jsonrpc": "2.0", "method": "experimental/serverStatus",
              "params": {"health": "ok", "quiescent": True}})
"#;

/// A wholly generic server: the same rename shape but only the two code
/// edits, and no adapter policy behind it.
const GENERIC_MOCK_BODY: &str = r#"
while True:
    msg = read_msg()
    if msg is None:
        break
    if "method" in msg and "id" in msg:
        method = msg["method"]
        if method == "initialize":
            reply(msg, {"capabilities": {
                "documentSymbolProvider": True, "renameProvider": True}})
        elif method == "textDocument/documentSymbol":
            reply(msg, [{"name": "greet", "kind": 12, "range": rng(5, 0, 6, 1),
                         "selectionRange": rng(5, 7, 5, 12),
                         "detail": None, "children": []}])
        elif method == "textDocument/definition":
            reply(msg, {"uri": msg["params"]["textDocument"]["uri"],
                        "range": rng(5, 7, 5, 12)})
        elif method == "textDocument/rename":
            uri = msg["params"]["textDocument"]["uri"]
            reply(msg, {"changes": {uri: [
                {"range": rng(2, 4, 2, 9), "newText": "salute"},
                {"range": rng(5, 7, 5, 12), "newText": "salute"}]}})
        else:
            reply(msg, None)
    elif msg.get("method") == "exit":
        sys.exit(0)
"#;

fn python() -> String {
    for candidate in [
        std::env::var("STRUCTURAL_TEST_PYTHON").ok(),
        Some("/opt/homebrew/bin/python3".into()),
        Some("/usr/local/bin/python3".into()),
        Some("/usr/bin/python3".into()),
        Some("python3".into()),
    ]
    .into_iter()
    .flatten()
    {
        if Command::new(&candidate)
            .arg("--version")
            .output()
            .map(|out| out.status.success())
            .unwrap_or(false)
        {
            return candidate;
        }
    }
    panic!("no fast python3 found for the mock server")
}

fn quoted(what: &str) -> String {
    format!("\"{}\"", what.replace('\\', "\\\\").replace('"', "\\\""))
}

/// One scenario per tag, so parallel tests never share a scratch root. The
/// built-in scenario overrides rust-analyzer's command and keeps its
/// adapter; the generic one is a wholly new server that serves rust.
fn scratch_root(tag: &str, mock_body: &str, builtin: bool) -> PathBuf {
    let root =
        std::env::temp_dir().join(format!("structural-doc-links-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/doc.rs"), DOC_RS).unwrap();
    let mock = root.join("mock.py");
    std::fs::write(&mock, format!("{PREAMBLE}{mock_body}")).unwrap();
    // The other rust-serving built-ins are pointed at commands that do not
    // exist, so exactly one server runs — built-ins are on by default and a
    // second answer would make every name ambiguous.
    let toml = if builtin {
        format!(
            "[servers.rust-analyzer]\ncommand = [{}, {}]\n\n\
             [servers.pyright]\ncommand = [\"/nonexistent/pyright\"]\n",
            quoted(&python()),
            quoted(&mock.display().to_string())
        )
    } else {
        format!(
            "[servers.generic]\ncommand = [{}, {}]\nlanguages = [\"rust\"]\n\n\
             [servers.rust-analyzer]\ncommand = [\"/nonexistent/rust-analyzer\"]\n\n\
             [servers.pyright]\ncommand = [\"/nonexistent/pyright\"]\n",
            quoted(&python()),
            quoted(&mock.display().to_string())
        )
    };
    std::fs::write(root.join("structural.toml"), toml).unwrap();
    std::fs::canonicalize(root).unwrap()
}

fn action(
    state: &structural::session::Session,
    new_name: &str,
    mode: Option<&str>,
    digest: Option<&str>,
) -> String {
    structural::tools::Rename {
        symbol: Some("greet".to_string()),
        path: Some("src/doc.rs".to_string()),
        anchor: None,
        focus: None,
        new_name: Some(new_name.to_string()),
        mode: mode.map(str::to_string),
        digest: digest.map(str::to_string),
        workspace: None,
        language: None,
    }
    .run(state)
    .unwrap_or_else(|error| panic!("rename greet → {new_name} ({mode:?}): {error:#}"))
}

fn digest_of(preview: &str) -> String {
    preview
        .lines()
        .find_map(|line| line.strip_prefix("digest: "))
        .unwrap_or_else(|| panic!("no digest in {preview}"))
        .to_string()
}

#[test]
fn a_mock_under_the_rust_analyzer_adapter_applies_its_doc_link_edit_and_carries_the_note() {
    let root = scratch_root("ra", RA_MOCK_BODY, true);
    let state = structural::session::Session::new(root.clone());

    let preview = action(&state, "salute", None, None);
    let note = "note: rust-analyzer rename may leave intra-doc links unchanged; \
                check doc comments for `greet`.\n"
        .to_string();
    let note = note.as_str();
    assert!(
        preview.starts_with("rename greet → salute · fn greet · src/doc.rs:6 · 3 edits · 1 file\n"),
        "{preview}"
    );
    // The doc-link edit is shown like any other edit — the caveat says
    // "may", so it is true whether or not this plan rewrote one.
    assert!(
        preview.contains(
            "  1: /// Links [`greet`] for the docs.\n     /// Links [`salute`] for the docs.\n"
        ),
        "{preview}"
    );
    assert!(
        preview.contains("  3:     greet()\n         salute()\n"),
        "{preview}"
    );
    assert!(preview.contains(note), "{preview}");
    let digest = digest_of(&preview);

    // A stale digest answers with a fresh preview: the caveat is on it too.
    let stale = action(&state, "salute", Some("apply"), Some("0000000000000000"));
    assert!(
        stale.starts_with("nothing applied: the digest does not match"),
        "{stale}"
    );
    assert!(
        stale.contains(note),
        "the fresh preview keeps the caveat: {stale}"
    );
    assert!(
        std::fs::read_to_string(root.join("src/doc.rs"))
            .unwrap()
            .contains("pub fn greet()"),
        "a stale apply writes nothing"
    );

    let applied = action(&state, "salute", Some("apply"), Some(&digest));
    assert!(
        applied.starts_with("applied: rename greet → salute · fn greet · src/doc.rs:6 · 3 edits · 1 file\nsrc/doc.rs · 3 edits\n"),
        "{applied}"
    );
    assert!(applied.ends_with(note), "{applied}");
    let text = std::fs::read_to_string(root.join("src/doc.rs")).unwrap();
    assert!(
        text.contains("/// Links [`salute`] for the docs."),
        "{text}"
    );
    assert!(text.contains("pub fn salute() -> usize {"), "{text}");
    assert!(text.contains("    salute()"), "{text}");

    drop(state);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_generic_server_gets_no_note_line_and_its_answer_is_applied_as_is() {
    let root = scratch_root("generic", GENERIC_MOCK_BODY, false);
    let state = structural::session::Session::new(root.clone());

    let preview = action(&state, "salute", None, None);
    assert!(
        preview.starts_with(
            "rename greet → salute · function greet · src/doc.rs:6 · 2 edits · 1 file\n"
        ),
        "{preview}"
    );
    assert!(!preview.contains("note:"), "{preview}");
    let applied = action(&state, "salute", Some("apply"), Some(&digest_of(&preview)));
    assert!(
        applied
            == "applied: rename greet → salute · function greet · src/doc.rs:6 · 2 edits · 1 file\nsrc/doc.rs · 2 edits\n",
        "{applied}"
    );
    let text = std::fs::read_to_string(root.join("src/doc.rs")).unwrap();
    assert!(
        text.contains("/// Links [`greet`] for the docs."),
        "a generic server's answer is applied as returned, doc link untouched: {text}"
    );
    assert!(text.contains("pub fn salute() -> usize {"), "{text}");

    drop(state);
    let _ = std::fs::remove_dir_all(&root);
}
