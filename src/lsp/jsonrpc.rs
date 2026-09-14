//! JSON-RPC message classification before serde collapses shapes.
//!
//! Promoted from the spike transport. The one non-obvious behavior lives here:
//! an explicit `"result": null` is a valid empty answer, while a response with
//! no `result` member at all is malformed. serde collapses both to `None`, so
//! the distinction is captured from the raw object *before* decoding.

use anyhow::Result;
use mcplease::types::{JsonRpcMessage, JsonRpcResponse, RequestId};
use serde_json::Value;

/// One message from the reader thread. Broken JSON does not kill the reader
/// (the frame boundary was still intact); a broken *frame* does, because the
/// stream cannot be re-synchronised from there.
pub enum Incoming {
    /// A decoded message, plus whether the raw JSON object had a `result`
    /// member — the explicit-null distinction described on the module.
    Message(Box<JsonRpcMessage>, bool),
    Malformed(String),
    Eof,
}

/// Decode one frame, validating the JSON-RPC response shape *before* serde
/// collapses an explicit `null` to `None`.
///
/// Returns the message and whether the raw object carried a `result` member. A
/// response must carry exactly one of `result` / `error`; neither (or both) is
/// malformed. This keeps a missing `result` from being silently read as a
/// successful null answer.
pub fn classify_frame(bytes: &[u8]) -> Result<(JsonRpcMessage, bool), String> {
    let value: Value =
        serde_json::from_slice(bytes).map_err(|error| format!("invalid JSON: {error}"))?;
    let object = value
        .as_object()
        .ok_or_else(|| "expected a JSON-RPC object".to_string())?;
    let has_result = object.contains_key("result");
    let has_error = object.contains_key("error");

    let message: JsonRpcMessage = serde_json::from_value(value)
        .map_err(|error| format!("invalid JSON-RPC message: {error}"))?;

    if matches!(message, JsonRpcMessage::Response(_)) {
        match (has_result, has_error) {
            (true, true) => return Err("response carries both `result` and `error`".into()),
            (false, false) => return Err("response carries neither `result` nor `error`".into()),
            _ => {}
        }
    }
    Ok((message, has_result))
}

/// Whether a decoded response is a success (explicit `null` included) rather
/// than an error response. `has_result` must come from [`classify_frame`].
pub fn response_result(
    response: JsonRpcResponse,
    has_result: bool,
) -> Result<Value, anyhow::Error> {
    let JsonRpcResponse {
        id: _,
        error,
        result,
        ..
    } = response;
    if let Some(error) = error {
        return Err(anyhow::anyhow!("{error}"));
    }
    if !has_result {
        return Err(anyhow::anyhow!("response had no `result` member"));
    }
    Ok(result.unwrap_or(Value::Null))
}

/// The success reply to a server-initiated request that takes no value, built
/// so the serialized frame carries `"result": null` (present, never omitted).
/// Used by the null-reply serialization test; the client's reply path goes
/// through `JsonRpcResponse::success` directly.
#[allow(dead_code)]
pub fn null_success(id: RequestId) -> JsonRpcResponse {
    JsonRpcResponse::success(id, Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn successful_null_reply_serializes_result_null() {
        // The reply to window/workDoneProgress/create is success(id, Null).
        // It must carry `"result":null`, never omit `result`.
        let message = JsonRpcMessage::Response(null_success(RequestId::Integer(7)));
        let text = serde_json::to_string(&message).unwrap();
        assert_eq!(text, r#"{"jsonrpc":"2.0","id":7,"result":null}"#);
    }

    #[test]
    fn explicit_null_result_is_a_valid_empty_answer() {
        // serde collapses a JSON null result to None; classify_frame preserves
        // that the member was present, so this is success with a null answer.
        let (message, has_result) =
            classify_frame(br#"{"jsonrpc":"2.0","id":7,"result":null}"#).unwrap();
        assert!(has_result);
        match message {
            JsonRpcMessage::Response(response) => {
                assert!(response.error.is_none());
                assert_eq!(response_result(response, has_result).unwrap(), Value::Null);
            }
            other => panic!("expected a response, got {other:?}"),
        }
    }

    #[test]
    fn error_response_is_an_error_even_with_explicit_null_result() {
        let (message, has_result) =
            classify_frame(br#"{"jsonrpc":"2.0","id":7,"error":{"code":-32603,"message":"boom"}}"#)
                .unwrap();
        assert!(!has_result);
        match message {
            JsonRpcMessage::Response(response) => {
                let error = response_result(response, has_result).unwrap_err();
                assert!(error.to_string().contains("boom"));
            }
            other => panic!("expected a response, got {other:?}"),
        }
    }

    #[test]
    fn response_with_neither_result_nor_error_is_malformed() {
        // Accepting this would turn a broken response into a successful null.
        let error = classify_frame(br#"{"jsonrpc":"2.0","id":7}"#).unwrap_err();
        assert!(
            error.contains("neither"),
            "expected a neither-result-nor-error rejection, got: {error}"
        );
    }

    #[test]
    fn response_with_both_result_and_error_is_malformed() {
        let error = classify_frame(
            br#"{"jsonrpc":"2.0","id":7,"result":1,"error":{"code":-1,"message":"x"}}"#,
        )
        .unwrap_err();
        assert!(
            error.contains("both"),
            "expected a both-result-and-error rejection, got: {error}"
        );
    }

    #[test]
    fn notifications_need_neither_result_nor_error() {
        // The exact-one-of rule applies to responses only.
        let (message, has_result) =
            classify_frame(br#"{"jsonrpc":"2.0","method":"$/progress","params":{}}"#).unwrap();
        assert!(!has_result);
        assert!(matches!(message, JsonRpcMessage::Notification(_)));
    }

    #[test]
    fn bare_json_null_is_not_a_json_rpc_object() {
        // A bare `null` on the wire is a protocol violation: it is reported
        // as malformed (the frame boundary survives; the reader continues).
        let error = classify_frame(b"null").unwrap_err();
        assert!(error.contains("expected a JSON-RPC object"), "{error}");
    }

    #[test]
    fn result_without_id_decodes_as_a_response_and_is_routed_as_stray() {
        // mcplease's response type permits a missing id, so a result-bearing
        // frame without one still classifies as a Response — and the client
        // routes it to the stray events (no waiter can be waiting on it,
        // since waiters wait on ids they were handed). The exact-one-of rule
        // still applies to it.
        let (message, has_result) = classify_frame(br#"{"jsonrpc":"2.0","result":null}"#).unwrap();
        assert!(has_result);
        assert!(matches!(message, JsonRpcMessage::Response(_)));
    }

    #[test]
    fn response_with_both_and_no_id_still_rejected() {
        let error =
            classify_frame(br#"{"jsonrpc":"2.0","result":1,"error":{"code":-1,"message":"x"}}"#)
                .unwrap_err();
        assert!(error.contains("both"), "{error}");
    }
}
