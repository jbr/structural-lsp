//! A file scope resolves from the file's own outline, deterministically and
//! without a real language server: a mock server whose `workspace/symbol`
//! knows nothing at all still resolves a qualified name through
//! `documentSymbol`, and its flat outline shape nominates from each
//! symbol's declaration line and the container the server names in a field.
//!
//! The configured built-ins point at commands that do not exist, so the
//! mock is the only server that runs; nothing here depends on rust-analyzer
//! or pyright being installed.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A language server that answers `workspace/symbol` with nothing, an
/// outline for `src/tool.rs` (hierarchical) and `src/flat.rs` (flat), and a
/// definition pointing at `new` in the file asked about. Everything else is
/// `null`.
pub(super) const MOCK_PY: &str = r#"
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

# The scratch file's shape: a struct, and the node a generic server
# reports for its class-like body, holding two methods (`src/tool.rs` in the
# test is this text). Its name and its range both say `Tool`, which is what
# the generic adapter's container rule requires.
TOOL_OUTLINE = [
    {"name": "Tool", "kind": 23, "range": rng(1, 0, 3, 1),
     "selectionRange": rng(1, 11, 1, 15), "detail": None, "children": []},
    {"name": "Tool", "kind": 19, "range": rng(5, 0, 13, 1),
     "selectionRange": rng(5, 5, 5, 9), "detail": None, "children": [
                           {"name": "new", "kind": 12, "range": rng(6, 4, 8, 5),
          "selectionRange": rng(6, 11, 6, 14), "detail": "fn(name: String) -> Self"},
         {"name": "name", "kind": 6, "range": rng(10, 4, 12, 5),
          "selectionRange": rng(10, 11, 10, 15), "detail": "fn(&self) -> &str"}]},
]

def outline(uri):
    if uri.endswith("/src/tool.rs"):
        return TOOL_OUTLINE
    if uri.endswith("/src/flat.rs"):
        # The flat shape: no selection range names a symbol.
        # The whole item, the way this shape reports it: no range names
        # the symbol, and the container is a field rather than nesting.
        return [{"name": "new", "kind": 12, "containerName": "Tool",
                 "location": {"uri": uri, "range": rng(6, 4, 8, 5)}}]
    if uri.endswith("/src/stale.rs"):
        # A stale outline: it advertises `new` under `Tool`, but the range it
        # gives for `new` is where `old` is written in the file.
        return [{"name": "Tool", "kind": 19, "range": rng(5, 0, 9, 1),
                 "selectionRange": rng(5, 5, 5, 9), "detail": None, "children": [
                     {"name": "new", "kind": 12, "range": rng(6, 4, 8, 5),
                      "selectionRange": rng(2, 4, 2, 7), "detail": "fn() -> Self"}]}]
    return None

while True:
    msg = read_msg()
    if msg is None:
        break
    if "method" in msg and "id" in msg:
        method = msg["method"]
        if method == "initialize":
            reply(msg, {"capabilities": {
                "documentSymbolProvider": True, "definitionProvider": True}})
        elif method == "workspace/symbol":
            # Never reached: this mock does not advertise
            # workspaceSymbolProvider, and structural does not send a
            # request a server did not offer. pylsp 1.15.0 answers exactly
            # this way, and the error used to end the whole query — a
            # file-scoped one included, which never needed the index.
            send({"jsonrpc": "2.0", "id": msg["id"], "error": {
                "code": -32601, "message": "Method Not Found: workspace/symbol"}})
        elif method == "textDocument/documentSymbol":
            reply(msg, outline(msg["params"]["textDocument"]["uri"]))
        elif method == "textDocument/definition":
            uri = msg["params"]["textDocument"]["uri"]
            reply(msg, {"uri": uri, "range": rng(6, 11, 6, 14)})
        else:
            reply(msg, None)
    elif "method" in msg and msg["method"] == "exit":
        sys.exit(0)
"#;

/// A dependency-free module: a struct, and an impl of it.
pub(super) const TOOL_RS: &str = "\
//! A scratch module.
pub struct Tool {
    name: String,
}

impl Tool {
    pub fn new(name: String) -> Self {
        Self { name }
    }

    pub fn name(&self) -> &str {
        &self.name
    }
}
";

/// A file the mock's outline has gone stale about: it answers `new` with the
/// range where `old` is written. Line 2, characters 4..7, is `old`.
const STALE_RS: &str = "\
//! A scratch module.
pub struct Tool {
    old: String,
}

impl Tool {
    pub fn new() -> Self {
        Self { old: String::new() }
    }
}
";

/// A Python 3 that starts fast enough for the launch backstop; the same
/// search `src/test_support.rs` makes, which an integration test cannot
/// reach.
pub(super) fn python() -> String {
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
            Command::new(candidate)
                .arg("--version")
                .output()
                .map(|output| output.status.success())
                .unwrap_or(false)
        })
        .map(str::to_string)
        .expect("a Python 3 to run the mock server; set STRUCTURAL_TEST_PYTHON")
}

pub(super) fn quoted(path: &Path) -> String {
    path.display()
        .to_string()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
}

fn scratch_root(name: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "structural-mock-scope-{}-{name}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/tool.rs"), TOOL_RS).unwrap();
    std::fs::write(root.join("src/flat.rs"), TOOL_RS).unwrap();
    std::fs::write(root.join("src/stale.rs"), STALE_RS).unwrap();
    std::fs::create_dir_all(root.join("web")).unwrap();
    std::fs::write(
        root.join("web/app.ts"),
        "export function app(): string {\n    return \"app\";\n}\n",
    )
    .unwrap();
    std::fs::write(root.join("web/app.py"), "def app():\n    return \"app\"\n").unwrap();
    let mock = root.join("mock.py");
    std::fs::write(&mock, MOCK_PY).unwrap();
    std::fs::write(
        root.join("structural.toml"),
        format!(
            concat!(
                "[servers.rust-analyzer]\ncommand = [\"/nonexistent/rust-analyzer\"]\n\n",
                "[servers.pyright]\ncommand = [\"/nonexistent/pyright\"]\n\n",
                // The machine's other measured python and typescript servers
                // are out of consideration: this fixture is about the miss
                // messages, and the servers it does have are the two above.
                // Without these, zero-configuration discovery would select
                // and start whatever the machine has installed
                // (docs/catalogue.md).
                "[servers.basedpyright]\ndisabled = true\n\n",
                "[servers.jedi]\ndisabled = true\n\n",
                "[servers.pyrefly]\ndisabled = true\n\n",
                "[servers.zuban]\ndisabled = true\n\n",
                "[servers.ty]\ndisabled = true\n\n",
                "[servers.pylsp]\ndisabled = true\n\n",
                "[servers.pylyzer]\ndisabled = true\n\n",
                "[servers.ruff]\ndisabled = true\n\n",
                "[servers.ruff-lsp]\ndisabled = true\n\n",
                "[servers.typescript-language-server]\ndisabled = true\n\n",
                "[servers.vtsls]\ndisabled = true\n\n",
                "[servers.deno]\ndisabled = true\n\n",
                "[servers.biome]\ndisabled = true\n\n",
                "[servers.vscode-eslint-language-server]\ndisabled = true\n\n",
                "[servers.mock]\ncommand = [\"{}\", \"{}\"]\nlanguages = [\"rust\"]\n"
            ),
            python(),
            quoted(&mock)
        ),
    )
    .unwrap();
    std::fs::canonicalize(root).unwrap()
}

fn show(
    state: &structural::session::Session,
    symbol: Option<&str>,
    path: Option<&str>,
) -> anyhow::Result<String> {
    structural::tools::Definition {
        symbol: symbol.map(str::to_string),
        path: path.map(str::to_string),
        anchor: None,
        focus: None,
        view: None,
        page: None,
        workspace: None,
        language: None,
    }
    .run(state)
}

#[test]
fn a_file_scope_resolves_without_the_symbol_index() {
    let root = scratch_root("file-scope");
    let state = structural::session::Session::new(root.clone());
    let ok = |result: anyhow::Result<String>| result.unwrap_or_else(|error| panic!("{error:#}"));

    // The index answered nothing at all; the file's own outline resolved the
    // qualified name, and the definition it navigated to is the body shown.
    let resolved = ok(show(&state, Some("Tool::new"), Some("src/tool.rs")));
    assert_eq!(
        resolved,
        "function new · src/tool.rs:7\npub fn new(name: String) -> Self {\n        Self { name }\n    }\n",
        "the file's own outline resolved the name the index never had"
    );

    // Without the path there is only the index, and it knows nothing about
    // the name: the file scope is what resolved it, and the miss says so
    // rather than leaving the caller to discover the scope.
    let bare = ok(show(&state, Some("Tool::new"), None));
    assert_eq!(
        bare,
        "no symbol named 'Tool::new'\n\
         note: no symbol index is available for rust; a name defined there resolves only when `path` names its file\n\
         note: the symbol index carries at most one container per symbol, and some servers report none; naming the declaring file in `path` resolves the qualifier from that file's outline instead\n"
    );

    // A server with no index answers every name the same way it answers an
    // absent one, so the miss says which languages had no index to ask. The
    // qualifier note is for a container the index could not narrow, and a
    // bare name that missed has nothing to relocate, so it does not carry
    // that one.
    let unqualified_miss = ok(show(&state, Some("Absent"), None));
    assert_eq!(
        unqualified_miss,
        "no symbol named 'Absent'\n\
         note: no symbol index is available for rust; a name defined there resolves only when `path` names its file\n"
    );

    // Naming a file is the thing the note asks for, so a miss inside one
    // does not repeat it.
    let in_file = ok(show(&state, Some("Absent"), Some("src/tool.rs")));
    assert_eq!(in_file, "no symbol named 'Absent'\n");

    // A qualifier the file's own symbols do not carry is listed from those
    // symbols, not substituted from the index.
    let wrong = ok(show(&state, Some("Missing::new"), Some("src/tool.rs")));
    assert_eq!(
        wrong,
        "no 'new' is in a container ending in `Missing`; the 'new' candidates:\n\
         function new · src/tool.rs:7 · in Tool\n\
         note: a qualifier matches the end of a candidate's container (the type, class or module enclosing it) or of its file's path, directories then file name (`tools::listing::render`); segments are names, so `crate`, `self` and `super` match nothing\n"
    );

    // A flat outline carries no range that names each symbol, so its
    // declaration line is searched for the advertised name, and the
    // container it names in a field is what the qualifier filters on.
    let flat = ok(show(&state, Some("Tool::new"), Some("src/flat.rs")));
    assert!(
        flat.starts_with("function new · src/flat.rs:7"),
        "a flat outline nominates from the declaration line: {flat}"
    );
    // The container is the server's claim, and a qualifier it does not
    // carry is refused from the file's own symbols rather than substituted.
    let flat_wrong = ok(show(&state, Some("Other::new"), Some("src/flat.rs")));
    assert_eq!(
        flat_wrong,
        "no 'new' is in a container ending in `Other`; the 'new' candidates:\n\
         function new · src/flat.rs:7 · in Tool\n\
         note: a qualifier matches the end of a candidate's container (the type, class or module enclosing it) or of its file's path, directories then file name (`tools::listing::render`); segments are names, so `crate`, `self` and `super` match nothing\n"
    );

    // A stale outline — the queried name advertised over a range that holds
    // another identifier — is an error, because the name is the server's
    // claim and the file is what checks it. Not a resolved answer, and not a
    // fallback to the index.
    let stale = show(&state, Some("Tool::new"), Some("src/stale.rs")).unwrap_err();
    let message = format!("{stale:#}");
    assert!(
        message.contains("does not describe the file as read from disk"),
        "{message}"
    );
    assert!(message.contains("found `old`"), "{message}");

    // The same candidate reached through a *listing* path instead of a
    // resolution: the qualifier matches nothing, so no definition is queried
    // and the unqualified listing would have been built from the stale range.
    // The check runs before any filter, so this errors too.
    let listing = show(&state, Some("Missing::new"), Some("src/stale.rs")).unwrap_err();
    let message = format!("{listing:#}");
    assert!(
        message.contains("does not describe the file as read from disk"),
        "{message}"
    );
    assert!(message.contains("found `old`"), "{message}");

    drop(state);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_directory_scope_over_unserved_languages_names_them() {
    let root = scratch_root("dir-scope");
    let state = structural::session::Session::new(root.clone());
    let ok = |result: anyhow::Result<String>| result.unwrap_or_else(|error| panic!("{error:#}"));

    // `web/` holds only TypeScript and Python, and the only running server
    // speaks Rust: the miss names the routing failure instead of reading as
    // an absent name.
    let miss = ok(show(&state, Some("Absent"), Some("web")));
    assert_eq!(
        miss,
        "no symbol named 'Absent'\n\
         note: no symbol index is available for rust; a name defined there resolves only when `path` names its file\n\
         note: `web/` holds `py`, `ts` files; no running language server serves `py`, `ts` files (running: mock)\n"
    );

    // `src/` holds only Rust, which the running server does serve: the same
    // miss carries no directory note.
    let served = ok(show(&state, Some("Absent"), Some("src")));
    assert_eq!(
        served,
        "no symbol named 'Absent'\n\
         note: no symbol index is available for rust; a name defined there resolves only when `path` names its file\n"
    );

    drop(state);
    let _ = std::fs::remove_dir_all(&root);
}

/// The same mock with a symbol index advertised and always empty: the
/// availability case, where the miss must name the silent server rather
/// than report the symbol absent.
///
/// Derived from [`MOCK_PY`] by string surgery so harness fixes land in
/// both; the asserts fail loudly if the surgery stops matching.
fn empty_py() -> String {
    let derived = MOCK_PY
        .replace(
            "\"documentSymbolProvider\": True, \"definitionProvider\": True}",
            "\"documentSymbolProvider\": True, \"definitionProvider\": True, \
             \"workspaceSymbolProvider\": True}",
        )
        .replace(
            "            # Never reached: this mock does not advertise\n            \
             # workspaceSymbolProvider, and structural does not send a\n            \
             # request a server did not offer. pylsp 1.15.0 answers exactly\n            \
             # this way, and the error used to end the whole query — a\n            \
             # file-scoped one included, which never needed the index.\n            \
             send({\"jsonrpc\": \"2.0\", \"id\": msg[\"id\"], \"error\": {\n                \
             \"code\": -32601, \"message\": \"Method Not Found: workspace/symbol\"}})",
            "            # Advertised and always empty: the availability case.\n            \
             # The index is offered and answers nothing, for every query.\n            \
             reply(msg, [])",
        );
    assert!(derived.contains("\"workspaceSymbolProvider\": True}"));
    assert!(derived.contains("reply(msg, [])"));
    assert!(!derived.contains("Method Not Found"));
    derived
}

#[test]
fn a_server_that_answers_nothing_is_named_on_the_miss() {
    let root = scratch_root("silent");
    std::fs::write(root.join("mock.py"), empty_py()).unwrap();
    let state = structural::session::Session::new(root.clone());
    let ok = |result: anyhow::Result<String>| result.unwrap_or_else(|error| panic!("{error:#}"));

    // The mock advertises an index and answers it empty, so there is no
    // no-index note — and the miss names the silent server instead of
    // reporting the symbol absent.
    let miss = ok(show(&state, Some("Absent"), None));
    assert_eq!(
        miss,
        "no symbol named 'Absent'\n\
         note: mock started but has answered nothing this session; the project may not be set up for `rust` files\n"
    );

    drop(state);
    let _ = std::fs::remove_dir_all(&root);
}

/// The same mock with a toggling header language: the index names the
/// definition in one file, and `definition` and `declaration` agree on the
/// declaration in the other. Both scratch files share the text, so every
/// range names its symbol either way.
///
/// Derived from [`empty_py`] the way it derives from [`MOCK_PY`]: the mock
/// learns file URIs from `documentSymbol` params (warm teaches it
/// `flat.rs`; a file-scoped query teaches it `tool.rs`), and the index
/// answers the learned `tool.rs` URI for `new`.
fn toggle_py() -> String {
    let base = empty_py()
        .replace(
            "\"workspaceSymbolProvider\": True}",
            "\"workspaceSymbolProvider\": True, \"declarationProvider\": True}",
        )
        .replace(
            "def outline(uri):",
            "URIS = []\n\
             \n\
             \n\
             def other_site(uri):\n    \
             # The definition of the name in one scratch file lives in the\n    \
             # other; both files share the text, so the range names it\n    \
             # either way.\n    \
             base = uri.rsplit(\"/\", 1)[0]\n    \
             other = \"flat.rs\" if uri.endswith(\"/src/tool.rs\") else \"tool.rs\"\n    \
             return {\"uri\": base + \"/\" + other, \"range\": rng(6, 11, 6, 14)}\n\
             \n\
             \n\
             def outline(uri):",
        )
        .replace(
            "        elif method == \"textDocument/documentSymbol\":\n            \
             reply(msg, outline(msg[\"params\"][\"textDocument\"][\"uri\"]))",
            "        elif method == \"textDocument/documentSymbol\":\n            \
             uri = msg[\"params\"][\"textDocument\"][\"uri\"]\n            \
             if uri not in URIS:\n                \
             URIS.append(uri)\n            \
             reply(msg, outline(uri))",
        )
        .replace(
            "            reply(msg, [])",
            "            query = msg[\"params\"][\"query\"]\n            \
             if query == \"new\" and URIS:\n                \
             tool = next((u for u in URIS if u.endswith(\"/src/tool.rs\")), URIS[0])\n                \
             reply(msg, [{\"name\": \"new\", \"kind\": 12, \"location\": \
             {\"uri\": tool, \"range\": rng(6, 11, 6, 14)}}])\n            \
             else:\n                \
             reply(msg, [])",
        )
        .replace(
            "        elif method == \"textDocument/definition\":\n            \
             uri = msg[\"params\"][\"textDocument\"][\"uri\"]\n            \
             reply(msg, {\"uri\": uri, \"range\": rng(6, 11, 6, 14)})",
            "        elif method == \"textDocument/definition\":\n            \
             reply(msg, other_site(msg[\"params\"][\"textDocument\"][\"uri\"]))\n        \
             elif method == \"textDocument/declaration\":\n            \
             reply(msg, other_site(msg[\"params\"][\"textDocument\"][\"uri\"]))",
        );
    assert!(base.contains("\"declarationProvider\": True}"));
    assert!(base.contains("def other_site(uri):"));
    assert!(base.contains("other_site(msg[\"params\"][\"textDocument\"][\"uri\"])"));
    assert!(base.contains("\"workspace/symbol\""));
    base
}

#[test]
fn a_server_that_answered_once_is_not_named() {
    let root = scratch_root("answered");
    std::fs::write(root.join("mock.py"), empty_py()).unwrap();
    let state = structural::session::Session::new(root.clone());
    let ok = |result: anyhow::Result<String>| result.unwrap_or_else(|error| panic!("{error:#}"));

    // The file's own outline resolved through this server, so it has shown
    // it serves: the later bare-name miss is absence, with no note.
    let resolved = ok(show(&state, Some("Tool::new"), Some("src/tool.rs")));
    assert_eq!(
        resolved,
        "function new · src/tool.rs:7\npub fn new(name: String) -> Self {\n        Self { name }\n    }\n",
    );

    let miss = ok(show(&state, Some("Absent"), None));
    assert_eq!(miss, "no symbol named 'Absent'\n");

    drop(state);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_bare_name_answers_the_definition_site() {
    let root = scratch_root("toggle");
    std::fs::write(root.join("mock.py"), toggle_py()).unwrap();
    let state = structural::session::Session::new(root.clone());
    let ok = |result: anyhow::Result<String>| result.unwrap_or_else(|error| panic!("{error:#}"));

    // File-scoped first: the outline resolves, and the mock learns
    // `tool.rs`'s URI for the index to answer later.
    let scoped = ok(show(&state, Some("Tool::new"), Some("src/tool.rs")));
    assert_eq!(
        scoped,
        "function new · src/tool.rs:7\npub fn new(name: String) -> Self {\n        Self { name }\n    }\n",
    );

    // Bare `new`: the index names `tool.rs`, while `definition` and
    // `declaration` agree on `flat.rs` — so the answer stays at the
    // definition with its body instead of following the hop to the
    // declaration's bare signature.
    let bare = ok(show(&state, Some("new"), None));
    assert_eq!(
        bare,
        "function new · src/tool.rs:7\npub fn new(name: String) -> Self {\n        Self { name }\n    }\n",
    );

    drop(state);
    let _ = std::fs::remove_dir_all(&root);
}
