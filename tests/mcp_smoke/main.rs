//! In-process and real-stdio MCP smokes for `show` and `uses`. Both need
//! rust-analyzer and are gated: run with `cargo test --test mcp_smoke -- --ignored`.

use mcplease::traits::Tool;
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

mod configured;
mod identity;
mod responsive;
mod servers;
mod stdio_server;
mod targeting;
use stdio_server::Server;

/// Copy a fixture tree, skipping build output.
fn copy_tree(from: &std::path::Path, to: &std::path::Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            if entry.file_name() == "target" {
                continue;
            }
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn rust_fixture_root() -> PathBuf {
    std::fs::canonicalize(concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/rust")).unwrap()
}

#[test]
#[ignore = "spawns rust-analyzer; run explicitly with -- --ignored"]
fn in_process_show_resolves_the_fixture_struct() {
    let mut state = structural::state::State::new(rust_fixture_root());
    // `choose` is unique in the fixture, so the positive path resolves.
    let output = structural::tools::Show {
        symbol: Some("choose".to_string()),
        path: None,
        anchor: None,
        focus: None,
    }
    .execute(&mut state, &mcplease::types::RequestContext::default())
    .expect("show choose succeeds");
    assert!(output.starts_with("fn choose · src/lib.rs:"), "{output}");
    assert!(output.contains("pub fn choose"), "{output}");
    assert!(
        output.contains("match mode"),
        "the fn body is included: {output}"
    );
    // Every byte is paid for: exactly one header line, then the item as it
    // sits in the file (rust-analyzer's extent includes the leading
    // annotation comment), and nothing else.
    let lib = std::fs::read_to_string(rust_fixture_root().join("src/lib.rs")).unwrap();
    let start = lib.find("// @decl choose | choose").unwrap();
    let end = start + lib[start..].find("\n}\n").unwrap() + 2;
    let line = lib[..lib.find("pub fn choose").unwrap()].lines().count() + 1;
    assert_eq!(
        output,
        format!("fn choose · src/lib.rs:{line}\n{}\n", &lib[start..end]),
        "one header line, the body, nothing else"
    );

    // A `path` scope narrows to the fixture file and still resolves.
    let scoped = structural::tools::Show {
        symbol: Some("choose".to_string()),
        path: Some("src/lib.rs".to_string()),
        anchor: None,
        focus: None,
    }
    .execute(&mut state, &mcplease::types::RequestContext::default())
    .expect("scoped show succeeds");
    assert!(scoped.starts_with("fn choose · src/lib.rs:"), "{scoped}");

    // The fixture's `Config` is declared once and re-imported in its tests
    // module; the adapter asks rust-analyzer to exclude imports from the
    // symbol search, so the bare name resolves to the struct.
    let config = structural::tools::Show {
        symbol: Some("Config".to_string()),
        path: None,
        anchor: None,
        focus: None,
    }
    .execute(&mut state, &mcplease::types::RequestContext::default())
    .expect("show Config succeeds");
    assert!(
        config.starts_with("struct Config · src/lib.rs:"),
        "{config}"
    );
    assert!(config.contains("pub struct Config"), "{config}");
    assert!(
        config.contains("#[derive(Debug, Clone)]"),
        "the attribute decorates the body: {config}"
    );

    // A missing name is a stated result, not an invented candidate.
    let missing = structural::tools::Show {
        symbol: Some("definitely_not_here".to_string()),
        path: None,
        anchor: None,
        focus: None,
    }
    .execute(&mut state, &mcplease::types::RequestContext::default())
    .unwrap();
    assert!(
        missing.starts_with("no symbol named 'definitely_not_here'"),
        "{missing}"
    );

    // A qualified name narrows by the container the server reports.
    let qualified = structural::tools::Show {
        symbol: Some("Config::new".to_string()),
        path: None,
        anchor: None,
        focus: None,
    }
    .execute(&mut state, &mcplease::types::RequestContext::default())
    .expect("show Config::new succeeds");
    assert!(qualified.starts_with("fn new · src/lib.rs:"), "{qualified}");

    // `uses` on the same fixture: every call to `choose` is in `run`, the
    // declaration is not a reference, and each hit carries its line.
    let uses = structural::tools::Uses {
        symbol: Some("choose".to_string()),
        path: None,
        anchor: None,
        focus: None,
    }
    .execute(&mut state, &mcplease::types::RequestContext::default())
    .expect("uses choose succeeds");
    let call_line = lib[..lib.find("let offset = choose(Mode::Fast);").unwrap()]
        .matches('\n')
        .count()
        + 1;
    assert_eq!(
        uses,
        format!(
            "fn choose · src/lib.rs:{line} · 1 reference · 1 file\nsrc/lib.rs\n  {call_line} · run · \
             let offset = choose(Mode::Fast);\n"
        ),
        "one header, one file, one hit, nothing else"
    );

    // `Config` is used as `Self` inside its impl: those are references to
    // the struct and verify through the adapter's alias, not an error. The
    // re-export in `alias` and the `use super::*` glob are not hits by
    // name; the impl headers and the constructor call are.
    let config_uses = structural::tools::Uses {
        symbol: Some("Config".to_string()),
        path: None,
        anchor: None,
        focus: None,
    }
    .execute(&mut state, &mcplease::types::RequestContext::default())
    .expect("uses Config succeeds");
    assert!(
        config_uses.starts_with("struct Config · src/lib.rs:"),
        "{config_uses}"
    );
    assert!(
        config_uses.contains("· new · pub fn new(limit: usize) -> Self {"),
        "{config_uses}"
    );
    assert!(
        config_uses.contains("· new · Self { limit }"),
        "{config_uses}"
    );
    assert!(
        config_uses.contains("· run · let config = Config::new(3);"),
        "{config_uses}"
    );
    assert!(config_uses.contains("· impl Config {"), "{config_uses}");
    assert!(
        config_uses.contains("· alias · pub use super::Config;"),
        "{config_uses}"
    );
    assert!(
        !config_uses.contains("pub struct Config"),
        "the declaration is not a reference: {config_uses}"
    );

    // A scope escaping the root is rejected.
    let escaping = structural::tools::Show {
        symbol: Some("Config".to_string()),
        path: Some("../semantic-edit-mcp".to_string()),
        anchor: None,
        focus: None,
    }
    .execute(&mut state, &mcplease::types::RequestContext::default());
    assert!(escaping.is_err(), "an out-of-root scope must be rejected");

    drop(state);
}

/// Without rust-analyzer on PATH the server must still come up, answer
/// `initialize` and `tools/list`, and turn `show` into an actionable
/// `isError` — never a panic and never silent. Runs everywhere: no r-a
/// needed, precisely because it is absent.
#[test]
fn stdio_mcp_serves_and_reports_actionable_error_without_rust_analyzer() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_structural"))
        .arg("serve")
        .env("STRUCTURAL_ROOT", rust_fixture_root())
        .env("PATH", "")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawning `structural serve` without rust-analyzer on PATH");
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());

    let mut request = |id: u64, method: &str, params: Value| -> Value {
        let frame = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        writeln!(stdin, "{frame}").unwrap();
        stdin.flush().unwrap();
        let mut line = String::new();
        loop {
            line.clear();
            let bytes = stdout.read_line(&mut line).expect("reading a response");
            assert!(
                bytes > 0,
                "server closed stdout before responding to id {id}"
            );
            let value: Value = serde_json::from_str(&line).expect("a JSON line");
            if value.get("id").and_then(Value::as_u64) == Some(id) {
                return value;
            }
        }
    };

    let initialize = request(
        1,
        "initialize",
        json!({
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": { "name": "smoke", "version": "0" }
        }),
    );
    assert!(initialize.get("result").is_some(), "{initialize}");

    let list = request(2, "tools/list", json!({}));
    let names: Vec<&str> = list
        .pointer("/result/tools")
        .and_then(Value::as_array)
        .expect("tools array")
        .iter()
        .filter_map(|tool| tool.get("name").and_then(Value::as_str))
        .collect();
    assert_eq!(names, ["show", "uses"], "{names:?}");

    let call = request(
        3,
        "tools/call",
        json!({ "name": "show", "arguments": { "symbol": "choose" } }),
    );
    assert_eq!(
        call.pointer("/result/isError"),
        Some(&json!(true)),
        "{call}"
    );
    let text = call
        .pointer("/result/content/0/text")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        text.contains("no language server is running"),
        "actionable error expected: {text}"
    );
    assert!(
        text.contains("rustup component add rust-analyzer"),
        "the error must name the fix: {text}"
    );
    assert!(
        text.contains("pyright"),
        "every configured server is accounted for: {text}"
    );

    let _ = writeln!(stdin, r#"{{"jsonrpc":"2.0","method":"exit"}}"#);
    let _ = child.kill();
    let _ = child.wait();
}

#[test]
#[ignore = "spawns rust-analyzer via the real server; run explicitly with -- --ignored"]
fn stdio_mcp_initialize_tools_list_and_show_round_trip() {
    let mut server = Server::start(rust_fixture_root().to_str().unwrap());

    let initialize = server.request(
        "initialize",
        json!({
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": { "name": "structural-smoke", "version": "0" }
        }),
    );
    let result = initialize.get("result").expect("initialize result");
    assert!(
        result.get("protocolVersion").is_some(),
        "handshake version echoed: {initialize}"
    );
    assert_eq!(
        result.pointer("/serverInfo/name").and_then(Value::as_str),
        Some("structural"),
        "{initialize}"
    );
    server.notify("notifications/initialized", json!({}));

    let list = server.request("tools/list", json!({}));
    let tools = list
        .pointer("/result/tools")
        .and_then(Value::as_array)
        .expect("tools array");
    let names: Vec<&str> = tools
        .iter()
        .filter_map(|tool| tool.get("name").and_then(Value::as_str))
        .collect();
    assert_eq!(names, ["show", "uses"], "the two tools: {names:?}");
    let schema = tools[0].get("inputSchema").expect("input schema");
    assert!(schema.to_string().contains("symbol"), "{schema}");

    // The real call: the server waits internally for rust-analyzer's
    // quiescence (cold start is ~10s here), then answers.
    let call = server.request(
        "tools/call",
        json!({ "name": "show", "arguments": { "symbol": "choose" } }),
    );
    let text = call
        .pointer("/result/content/0/text")
        .and_then(Value::as_str)
        .expect("tool text content");
    assert!(
        call.pointer("/result/isError") != Some(&json!(true)),
        "the successful show call must not be an error: {call}"
    );
    assert!(text.starts_with("fn choose · src/lib.rs:"), "{text}");
    assert!(text.contains("pub fn choose"), "{text}");

    // The re-imported struct resolves once: imports are excluded from the
    // symbol search by the adapter.
    let config = server.request(
        "tools/call",
        json!({ "name": "show", "arguments": { "symbol": "Config" } }),
    );
    let text = config
        .pointer("/result/content/0/text")
        .and_then(Value::as_str)
        .expect("tool text content");
    assert!(text.starts_with("struct Config · src/lib.rs:"), "{text}");
    assert!(text.contains("pub struct Config"), "{text}");

    // The qualified name resolves through the same path.
    let qualified = server.request(
        "tools/call",
        json!({ "name": "show", "arguments": { "symbol": "Config::new" } }),
    );
    let text = qualified
        .pointer("/result/content/0/text")
        .and_then(Value::as_str)
        .expect("tool text content");
    assert!(text.starts_with("fn new · src/lib.rs:"), "{qualified}");

    // ping round-trips.
    let pong = server.request("ping", json!({}));
    assert!(pong.get("result").is_some(), "{pong}");
}

#[test]
#[ignore = "spawns rust-analyzer via the real server; run explicitly with -- --ignored"]
fn stdio_mcp_show_refreshes_a_document_changed_on_disk() {
    // A scratch copy of the fixture root, so the on-disk edit is safe.
    let scratch = std::env::temp_dir().join(format!("structural-smoke-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch).unwrap();
    let source_root = rust_fixture_root();
    for entry in ["Cargo.toml", "src"] {
        let target = scratch.join(entry);
        if entry == "src" {
            std::fs::create_dir_all(&target).unwrap();
        }
        let source = source_root.join(entry);
        if source.is_dir() {
            for file in std::fs::read_dir(&source).unwrap() {
                let file = file.unwrap();
                std::fs::copy(file.path(), target.join(file.file_name())).unwrap();
            }
        } else {
            std::fs::copy(&source, &target).unwrap();
        }
    }

    let mut server = Server::start(scratch.to_str().unwrap());
    server.request(
        "initialize",
        json!({
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": { "name": "structural-smoke", "version": "0" }
        }),
    );
    server.notify("notifications/initialized", json!({}));

    let before = server.request(
        "tools/call",
        json!({ "name": "show", "arguments": { "symbol": "choose" } }),
    );
    let text = before
        .pointer("/result/content/0/text")
        .and_then(Value::as_str)
        .unwrap();
    assert!(text.contains("pub fn choose"), "{text}");

    // Change the body on disk, then ask again: the tool must serve the new
    // text, not a stale snapshot.
    let lib_path = scratch.join("src/lib.rs");
    let updated = std::fs::read_to_string(&lib_path).unwrap().replace(
        "match mode { Mode::Fast => 1, Mode::Safe => 2 }",
        "match mode { Mode::Fast => 10, Mode::Safe => 20 }",
    );
    std::fs::write(&lib_path, updated).unwrap();

    let after = server.request(
        "tools/call",
        json!({ "name": "show", "arguments": { "symbol": "choose" } }),
    );
    let text = after
        .pointer("/result/content/0/text")
        .and_then(Value::as_str)
        .unwrap();
    assert!(
        text.contains("Mode::Fast => 10"),
        "refreshed body expected: {text}"
    );
    assert!(
        !text.contains("Mode::Fast => 1,"),
        "stale body must not be served: {text}"
    );

    server.child.kill().ok();
    let _ = std::fs::remove_dir_all(&scratch);
}

/// Freshness regression (review item 1): a previously opened document is
/// refreshed BEFORE discovery, so inserting lines before a chosen declaration
/// re-resolves the range; a renamed symbol is reported as absent rather than
/// resolved from a stale index; and a second module edited between calls is
/// re-fed before its next query. All against real rust-analyzer on a scratch
/// copy of the fixture.
#[test]
#[ignore = "spawns rust-analyzer; run explicitly with -- --ignored"]
fn in_process_show_stays_fresh_across_on_disk_edits() {
    // Scratch fixture with a second module: src/util.rs, `mod util;` in lib.
    let scratch = std::env::temp_dir().join(format!("structural-fresh-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(scratch.join("src")).unwrap();
    let source_root = rust_fixture_root();
    std::fs::copy(source_root.join("Cargo.toml"), scratch.join("Cargo.toml")).unwrap();
    let lib_text = std::fs::read_to_string(source_root.join("src/lib.rs")).unwrap();
    std::fs::write(
        scratch.join("src/lib.rs"),
        format!(
            "mod util;\n\n{lib_text}\n// keep util referenced: {anchor}\n",
            anchor = "pub fn uses_helper(x: usize) -> usize { util::helper(x) }"
        ),
    )
    .unwrap();
    std::fs::write(
        scratch.join("src/util.rs"),
        "pub fn helper(x: usize) -> usize {\n    x + 1\n}\n",
    )
    .unwrap();

    let mut state = structural::state::State::new(scratch.clone());
    let show = |state: &mut structural::state::State, symbol: &str| {
        structural::tools::Show {
            symbol: Some(symbol.to_string()),
            path: None,
            anchor: None,
            focus: None,
        }
        .execute(state, &mcplease::types::RequestContext::default())
    };

    // Call 1 opens both files the tool touches (lib.rs as candidate).
    let first = show(&mut state, "choose").expect("first show resolves");
    assert!(first.contains("pub fn choose"), "{first}");
    let helper_first = show(&mut state, "helper").expect("helper resolves cross-module");
    assert!(helper_first.contains("pub fn helper"), "{helper_first}");

    let lib_path = scratch.join("src/lib.rs");
    let util_path = scratch.join("src/util.rs");

    // Edit 1: insert 10 lines before `pub fn choose` in lib.rs (shifts its
    // declaration down).
    let shifted = std::fs::read_to_string(&lib_path)
        .unwrap()
        .replacen("pub fn choose", "// lead\n// lines\n// to\n// shift\n// the\n// index\n// down\n// by\n// several\n// lines\npub fn choose", 1);
    assert_ne!(shifted, std::fs::read_to_string(&lib_path).unwrap());
    std::fs::write(&lib_path, shifted).unwrap();

    let second = show(&mut state, "choose").expect("show after shift resolves");
    assert!(
        second.contains("pub fn choose"),
        "the function must still be found and shaped after the shift: {second}"
    );
    assert!(
        second.contains("match mode"),
        "the shifted body is served, not a stale-range neighbor item: {second}"
    );

    // Edit 2: rename the symbol on disk. The stale-index answer would be a
    // confident body; freshness means the index is re-fed first, so the name
    // is no longer found.
    let renamed =
        std::fs::read_to_string(&lib_path)
            .unwrap()
            .replacen("pub fn choose", "pub fn chooser", 1);
    std::fs::write(&lib_path, renamed).unwrap();

    let third = show(&mut state, "choose").expect("the rename case is a stated result");
    // The renamed symbol may come back as a fuzzy near-match, but the exact
    // name must not resolve: nothing was resolved, no definition was queried.
    assert!(
        third.starts_with("no exact match for 'choose'")
            || third.starts_with("no symbol named 'choose'"),
        "a renamed symbol must not resolve from a stale index: {third}"
    );
    assert!(!third.starts_with("fn choose"), "{third}");

    // Edit 3: change the previously opened second module; the next query must
    // serve the new text (the doc was opened in the helper call above).
    let edited = std::fs::read_to_string(&util_path)
        .unwrap()
        .replace("x + 1", "x + 100");
    std::fs::write(&util_path, edited).unwrap();

    let helper_second = show(&mut state, "helper").expect("helper re-resolves");
    assert!(
        helper_second.contains("x + 100"),
        "the edited module body is served fresh: {helper_second}"
    );
    assert!(
        !helper_second.contains("x + 1\n"),
        "the stale body must not be served: {helper_second}"
    );

    drop(state);
    let _ = std::fs::remove_dir_all(&scratch);
}
