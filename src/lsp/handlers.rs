//! Replies to server-initiated LSP requests, as a pure decision.
//!
//! An LSP server sends requests *to* the client that must be answered. This
//! module decides the reply for each method with no I/O, so the policy is
//! testable without a process; [`super::client::Client`] only serializes what
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
) -> Reply {
    match method {
        // One entry per requested item; null means "no opinion", which hands
        // the server its own defaults.
        "workspace/configuration" => {
            let items = params
                .and_then(|p| p.get("items"))
                .and_then(Value::as_array)
                .map(Vec::len)
                .unwrap_or(0);
            Reply::Success(Value::Array(vec![Value::Null; items]))
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

    #[test]
    fn configuration_answers_one_null_per_item() {
        let params = json!({ "items": [{}, {}] });
        assert_eq!(
            server_request_reply("workspace/configuration", Some(&params), &folders()),
            Reply::Success(json!([Value::Null, Value::Null]))
        );
    }

    #[test]
    fn configuration_without_items_answers_an_empty_array() {
        assert_eq!(
            server_request_reply("workspace/configuration", None, &folders()),
            Reply::Success(json!([]))
        );
    }

    #[test]
    fn progress_create_answers_explicit_null() {
        assert_eq!(
            server_request_reply("window/workDoneProgress/create", None, &folders()),
            Reply::Success(Value::Null)
        );
    }

    #[test]
    fn workspace_folders_answers_the_offered_folders() {
        assert_eq!(
            server_request_reply("workspace/workspaceFolders", None, &folders()),
            Reply::Success(folders())
        );
    }

    #[test]
    fn register_and_unregister_answer_null() {
        for method in ["client/registerCapability", "client/unregisterCapability"] {
            assert_eq!(
                server_request_reply(method, None, &folders()),
                Reply::Success(Value::Null)
            );
        }
    }

    #[test]
    fn apply_edit_is_always_declined_regardless_of_payload() {
        let params =
            json!({ "edit": { "changes": { "file:///tmp/ws/a.rs": [{ "newText": "x" }] } } });
        assert_eq!(
            server_request_reply("workspace/applyEdit", Some(&params), &folders()),
            Reply::Success(json!({ "applied": false }))
        );
    }

    #[test]
    fn unknown_methods_get_method_not_found() {
        // `JsonRpcError::method_not_found`'s own message wording.
        assert_eq!(
            server_request_reply("test/unknown", None, &folders()),
            Reply::Error(-32601, "unknown method: test/unknown".to_string())
        );
    }
}
