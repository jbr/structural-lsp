//! The operator channel through the real front end: a launch failure on
//! stderr and as MCP logging, never in a tool answer it did not already
//! belong to (`docs/operator.md`).

use crate::{Server, rust_fixture_root};
use serde_json::{Value, json};

/// With no rust-analyzer on PATH the eager warm fails before any client is
/// connected. The failure is written to stderr at once, held for the
/// connection, and sent as a `notifications/message` once the client has
/// completed its handshake and called a tool — ahead of that call's
/// answer. Runs everywhere: no server is needed, because it is absent.
#[test]
fn stdio_mcp_reports_a_launch_failure_on_stderr_and_as_logging() {
    let mut server = Server::start_with_env(
        rust_fixture_root().to_str().unwrap(),
        &[("PATH", String::new())],
    );
    let line = server.event("structural: error:");
    assert!(
        line.contains(": rust-analyzer: failed to start:") && line.contains("to fix:"),
        "{line}"
    );

    let initialize = server.request(
        "initialize",
        json!({
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": { "name": "smoke", "version": "0" }
        }),
    );
    assert_eq!(
        initialize.pointer("/result/capabilities/logging"),
        Some(&json!({})),
        "{initialize}"
    );
    server.notify("notifications/initialized", json!({}));
    let set = server.request("logging/setLevel", json!({ "level": "warning" }));
    assert_eq!(set.get("result"), Some(&json!({})), "{set}");

    let call = server.request(
        "tools/call",
        json!({ "name": "definition", "arguments": { "symbol": "choose" } }),
    );
    assert_eq!(
        call.pointer("/result/isError"),
        Some(&json!(true)),
        "{call}"
    );
    let messages: Vec<Value> = server
        .take_skipped()
        .into_iter()
        .filter(|frame| frame.get("method") == Some(&json!("notifications/message")))
        .collect();
    assert_eq!(messages.len(), 1, "{messages:?}");
    let params = &messages[0]["params"];
    assert_eq!(params["level"], "error", "{params}");
    assert_eq!(params["logger"], "structural", "{params}");
    assert_eq!(params["data"]["kind"], "launch-failed", "{params}");
    assert_eq!(params["data"]["server"], "rust-analyzer", "{params}");
    assert_eq!(
        params["data"]["root"],
        rust_fixture_root().display().to_string(),
        "{params}"
    );
    assert!(
        params["data"]["message"]
            .as_str()
            .is_some_and(|message| message.starts_with("failed to start:")),
        "{params}"
    );

    server.close_stdin();
    assert!(server.wait_exit().success());
}
