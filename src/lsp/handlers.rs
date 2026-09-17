//! Replies to server-initiated LSP requests, as a pure decision.
//!
//! An LSP server sends requests *to* the client that must be answered. This
//! module decides the reply for each method with no I/O, so the policy is
//! testable without a process; the transport only serializes what
//! this function returns.
//!
//! The spike is read-only and so is the MCP slice: the one request that could
//! mutate, `workspace/applyEdit`, is always declined with `{"applied": false}`,
//! no matter what edit it carries.

use mcplease::types::JsonRpcError;
use serde_json::{Value, json};

/// The reply to one server-initiated request.
#[derive(Debug, PartialEq)]
pub enum Reply {
    /// A success response with this `result` value (explicit null included —
    /// the serialized frame must carry `"result": null`, never omit it).
    Success(Value),
    /// An error response with this code and message.
    Error(i64, String),
}

impl Reply {
    pub fn method_not_found(method: &str) -> Self {
        let error = JsonRpcError::method_not_found(method);
        Self::Error(error.code, error.message)
    }
}

/// Decide the reply for one server-initiated request.
///
/// `workspace_folders` is what the client offered at `initialize`: a client
/// that advertises `workspace.workspaceFolders` has promised to answer this
/// request with those folders, not with null and not with MethodNotFound.
pub fn server_request_reply(
    method: &str,
    params: Option<&Value>,
    workspace_folders: &Value,
    configuration: &Value,
) -> Reply {
    match method {
        // One entry per requested item. `Null` is "no opinion", which hands
        // the server its own defaults — and for some servers the default is
        // to serve nothing: deno 2.9.6 answers every request empty for the
        // whole session unless this carries `{"enable": true}`
        // (docs/servers.md).
        //
        // The same value answers every item, whatever `section` each names.
        // No server measured here wants different settings per section, and
        // a per-section table would be a shape invented ahead of a case for
        // it; `section` is available in `params` when one turns up.
        "workspace/configuration" => {
            let items = params
                .and_then(|p| p.get("items"))
                .and_then(Value::as_array)
                .map(Vec::len)
                .unwrap_or(0);
            Reply::Success(Value::Array(vec![configuration.clone(); items]))
        }
        "window/workDoneProgress/create" => Reply::Success(Value::Null),
        "workspace/workspaceFolders" => Reply::Success(workspace_folders.clone()),
        "client/registerCapability" | "client/unregisterCapability" => Reply::Success(Value::Null),
        // Always declined, never applied. The spike's contract, kept.
        "workspace/applyEdit" => Reply::Success(json!({ "applied": false })),
        other => Reply::method_not_found(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folders() -> Value {
        json!([{ "uri": "file:///tmp/ws", "name": "ws" }])
    }

    /// No configured settings: the reply that hands a server its defaults.
    fn no_opinion() -> Value {
        Value::Null
    }

    #[test]
    fn configuration_answers_one_entry_per_item() {
        let params = json!({ "items": [{}, {}] });
        assert_eq!(
            server_request_reply(
                "workspace/configuration",
                Some(&params),
                &folders(),
                &no_opinion()
            ),
            Reply::Success(json!([Value::Null, Value::Null]))
        );
    }

    /// Configured settings answer every item, whatever section it names.
    /// deno 2.9.6 serves nothing for the whole session without this, and
    /// ignores the same value in the initialization options
    /// (docs/servers.md).
    #[test]
    fn configured_settings_answer_every_requested_section() {
        let params = json!({ "items": [{ "section": "deno" }, { "section": "typescript" }] });
        let settings = json!({ "enable": true });
        assert_eq!(
            server_request_reply(
                "workspace/configuration",
                Some(&params),
                &folders(),
                &settings
            ),
            Reply::Success(json!([{ "enable": true }, { "enable": true }]))
        );
    }

    #[test]
    fn configuration_without_items_answers_an_empty_array() {
        assert_eq!(
            server_request_reply("workspace/configuration", None, &folders(), &no_opinion()),
            Reply::Success(json!([]))
        );
    }

    #[test]
    fn progress_create_answers_explicit_null() {
        assert_eq!(
            server_request_reply(
                "window/workDoneProgress/create",
                None,
                &folders(),
                &no_opinion()
            ),
            Reply::Success(Value::Null)
        );
    }

    #[test]
    fn workspace_folders_answers_the_offered_folders() {
        assert_eq!(
            server_request_reply(
                "workspace/workspaceFolders",
                None,
                &folders(),
                &no_opinion()
            ),
            Reply::Success(folders())
        );
    }

    #[test]
    fn register_and_unregister_answer_null() {
        for method in ["client/registerCapability", "client/unregisterCapability"] {
            assert_eq!(
                server_request_reply(method, None, &folders(), &no_opinion()),
                Reply::Success(Value::Null)
            );
        }
    }

    #[test]
    fn apply_edit_is_always_declined_regardless_of_payload() {
        let params =
            json!({ "edit": { "changes": { "file:///tmp/ws/a.rs": [{ "newText": "x" }] } } });
        assert_eq!(
            server_request_reply(
                "workspace/applyEdit",
                Some(&params),
                &folders(),
                &no_opinion()
            ),
            Reply::Success(json!({ "applied": false }))
        );
    }

    #[test]
    fn unknown_methods_get_method_not_found() {
        // `JsonRpcError::method_not_found`'s own message wording.
        assert_eq!(
            server_request_reply("test/unknown", None, &folders(), &no_opinion()),
            Reply::Error(-32601, "unknown method: test/unknown".to_string())
        );
    }
}
