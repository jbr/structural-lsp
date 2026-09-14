//! Throwaway synchronous stdio LSP client for the raw spike.
//!
//! This is deliberately *not* the production shape. The design's eventual
//! transport is a sans-IO protocol state machine beside a framing-only pipe;
//! this is the opposite: a blocking client that owns a child process, a
//! stdout reader thread, and an `mpsc` channel, and does exactly one thing
//! well enough to gather raw server responses.
//!
//! ## What is reused from mcplease, and what is not
//!
//! Reused as-is (`mcplease::types`): the JSON-RPC envelope types
//! [`JsonRpcRequest`], [`JsonRpcNotification`], [`JsonRpcResponse`],
//! [`JsonRpcError`], [`RequestId`], and [`JsonRpcMessage`] with its
//! method/id/result shape discrimination. LSP and MCP are both JSON-RPC 2.0,
//! so the envelopes are the same objects.
//!
//! Not reused, and why:
//!
//! - **Framing.** mcplease's `stdio::serve` is newline-delimited JSON (MCP's
//!   stdio transport); LSP frames with a `Content-Length:` header block. Its
//!   `headers` module is HTTP header validation, also not LSP framing. So the
//!   [`read_frame`] / [`write_frame`] pair here is hand-written.
//! - **`client::ClientProtocol` / `Reaction` / `Negotiation`.** That layer is
//!   MCP-2026-07-28-specific: `_meta`, protocol-version negotiation, tools and
//!   capabilities. It has the right *shape* (no I/O, classify incoming
//!   messages) but the wrong semantics for LSP, which negotiates differently
//!   and has no `_meta`. Only the envelope types carry over cleanly.
//!
//! ## Server-initiated messages
//!
//! An LSP server sends requests *to* the client that must be answered
//! (`workspace/configuration`, `window/workDoneProgress/create`,
//! `client/registerCapability`, `workspace/applyEdit`) and a steady stream of
//! notifications. This client answers the four requests above and records
//! every server request and notification in [`Client::events`]. The spike
//! never mutates: `workspace/applyEdit` is answered `{"applied": false}`.

use anyhow::{Context, Result, anyhow, bail};
use mcplease::types::{
    JsonRpcError, JsonRpcMessage, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse, RequestId,
};
use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// Upper bound on a single frame body, so a corrupt or hostile
/// `Content-Length` cannot make the reader allocate unbounded memory. 64 MiB
/// is far above any real LSP payload the spike produces (references lists
/// included).
pub const MAX_FRAME_BYTES: usize = 64 * 1024 * 1024;

/// Upper bound on the header block, and on the number of header lines in it.
/// Both are tiny in practice; the caps exist so a hostile or wedged server
/// cannot make the client allocate without limit before the body cap applies.
pub const MAX_HEADER_BYTES: usize = 8 * 1024;
pub const MAX_HEADERS: usize = 64;

/// One message from the reader thread. Broken JSON does not kill the reader
/// (the frame boundary was still intact); a broken *frame* does, because the
/// stream cannot be re-synchronised from there.
enum Incoming {
    /// A decoded message, plus whether the raw JSON object had a `result`
    /// member. The flag distinguishes an explicit `"result": null` (a valid
    /// empty answer) from a response with no `result` at all; serde collapses
    /// both to `None`, so the distinction is captured before that.
    Message(Box<JsonRpcMessage>, bool),
    Malformed(String),
    Eof,
}

/// A running language server over stdio, addressed synchronously.
pub struct Client {
    child: Child,
    stdin: Option<ChildStdin>,
    rx: Receiver<Incoming>,
    reader: Option<JoinHandle<()>>,
    reader_done: Arc<AtomicBool>,
    next_id: i64,
    /// Every notification and server request seen so far, plus any response
    /// that did not belong to the call currently waiting. Notifications are
    /// stored as `{"method": ..., "params": ...}`.
    pub events: Vec<Value>,
    /// When `STRUCTURAL_SPIKE_WIRE=1`, log every incoming frame's shape to
    /// stderr. Diagnostic only; used to explain a server that answers some
    /// methods and hangs on others.
    debug: bool,
    /// The workspace folders captured from the `initialize` params, so a
    /// server-initiated `workspace/workspaceFolders` request can be answered
    /// with them (advertising `workspace.workspaceFolders` promises this).
    workspace_folders: Value,
}

impl Client {
    /// Spawn `command` with `args` and start reader/writer plumbing. stderr is
    /// inherited so server logs reach the terminal.
    pub fn spawn(command: &str, args: &[String]) -> Result<Self> {
        let mut child = Command::new(command)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .with_context(|| format!("spawning `{command}`"))?;

        let stdin = child.stdin.take().context("child stdin was not piped")?;
        let stdout = child.stdout.take().context("child stdout was not piped")?;
        let (tx, rx) = mpsc::channel();
        let reader_done = Arc::new(AtomicBool::new(false));
        let reader = thread::Builder::new()
            .name("lsp-reader".into())
            .spawn({
                let done = Arc::clone(&reader_done);
                move || {
                    reader_loop(BufReader::new(stdout), tx);
                    done.store(true, Ordering::SeqCst);
                }
            })
            .context("spawning reader thread")?;

        Ok(Self {
            child,
            stdin: Some(stdin),
            rx,
            reader: Some(reader),
            reader_done,
            next_id: 1,
            events: Vec::new(),
            debug: std::env::var("STRUCTURAL_SPIKE_WIRE").is_ok_and(|value| value != "0"),
            workspace_folders: Value::Null,
        })
    }

    /// Send a request and block until its response arrives, pumping any
    /// interleaved notifications and server requests meanwhile. Errors on
    /// timeout, EOF, a reader death, or an error response.
    pub fn request(&mut self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        if method == "initialize" {
            // Remember the folders the client offered; a server that sees the
            // `workspace.workspaceFolders` capability may later ask for them.
            self.workspace_folders = params
                .get("workspaceFolders")
                .cloned()
                .unwrap_or(Value::Null);
        }
        let id = RequestId::Integer(self.next_id);
        self.next_id += 1;
        self.send(&JsonRpcMessage::Request(JsonRpcRequest::new(
            id.clone(),
            method,
            Some(params),
        )))?;

        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline
                .checked_duration_since(Instant::now())
                .filter(|d| !d.is_zero())
                .ok_or_else(|| {
                    anyhow!(
                        "timed out after {timeout:?} waiting for response to {method} (id {id:?})"
                    )
                })?;
            let incoming = self.recv(remaining, method)?;
            if let Some(result) = self.dispatch(incoming, Some(&id), method)? {
                return Ok(result);
            }
        }
    }

    /// Send a one-way notification.
    pub fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        self.send(&JsonRpcMessage::Notification(JsonRpcNotification::new(
            method,
            Some(params),
        )))
    }

    /// Process everything that arrives for up to `timeout`, collecting it in
    /// [`Self::events`], then return. Used to wait for readiness signals and
    /// to drain server chatter between queries.
    pub fn pump(&mut self, timeout: Duration) -> Result<()> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = match deadline.checked_duration_since(Instant::now()) {
                Some(d) if !d.is_zero() => d,
                _ => return Ok(()),
            };
            match self.rx.recv_timeout(remaining) {
                Ok(incoming) => {
                    let _ = self.dispatch(incoming, None, "pump")?;
                }
                Err(RecvTimeoutError::Timeout) => return Ok(()),
                Err(RecvTimeoutError::Disconnected) => {
                    bail!("LSP reader thread stopped unexpectedly during pump")
                }
            }
        }
    }

    /// Ask the server to shut down, tell it to exit, then kill and reap it.
    /// Errors from the shutdown handshake are swallowed: the child is killed
    /// regardless.
    pub fn shutdown(&mut self) -> Result<()> {
        let _ = self.request("shutdown", Value::Null, Duration::from_secs(10));
        let _ = self.notify("exit", Value::Null);
        self.cleanup();
        Ok(())
    }

    fn send(&mut self, message: &JsonRpcMessage) -> Result<()> {
        let bytes = serde_json::to_vec(message).context("serializing JSON-RPC message")?;
        if self.debug {
            // Full serialized frame, so a malformed reply is visible on the
            // wire rather than only as a debug summary.
            let text = String::from_utf8_lossy(&bytes);
            let shown = if text.chars().count() > 240 {
                format!("{}…", text.chars().take(240).collect::<String>())
            } else {
                text.into_owned()
            };
            eprintln!("[lsp>] {shown}");
        }
        let stdin = self.stdin.as_mut().context("client stdin already closed")?;
        write_frame(stdin, &bytes)
    }

    fn recv(&mut self, timeout: Duration, context: &str) -> Result<Incoming> {
        self.rx.recv_timeout(timeout).map_err(|e| match e {
            RecvTimeoutError::Timeout => {
                anyhow!("timed out after {timeout:?} while waiting in {context}")
            }
            RecvTimeoutError::Disconnected => {
                anyhow!("LSP reader thread stopped unexpectedly during {context}")
            }
        })
    }

    /// Route one incoming message. Returns `Some(result)` only when it is the
    /// response to `want`.
    fn dispatch(
        &mut self,
        incoming: Incoming,
        want: Option<&RequestId>,
        context: &str,
    ) -> Result<Option<Value>> {
        if self.debug {
            let summary = match &incoming {
                Incoming::Message(message, _) => match &**message {
                    JsonRpcMessage::Request(request) => format!("request {}", request.method),
                    JsonRpcMessage::Notification(notification) => {
                        match notification.method.as_str() {
                            "window/logMessage" | "window/showMessage" => format!(
                                "notification {} {}",
                                notification.method,
                                notification
                                    .params
                                    .as_ref()
                                    .map(|p| p.to_string())
                                    .unwrap_or_default()
                            ),
                            _ => format!("notification {}", notification.method),
                        }
                    }
                    JsonRpcMessage::Response(response) => format!("response id {:?}", response.id),
                },
                Incoming::Malformed(error) => format!("MALFORMED {error}"),
                Incoming::Eof => "EOF".to_string(),
            };
            eprintln!("[lsp<] {summary}");
        }
        match incoming {
            Incoming::Eof => bail!("language server closed stdout during {context}"),
            Incoming::Malformed(err) => {
                self.events.push(json!({ "unparsable": err }));
                Ok(None)
            }
            Incoming::Message(message, has_result) => match *message {
                JsonRpcMessage::Response(response) => {
                    if response.id.as_ref().is_some_and(|id| Some(id) == want) {
                        // `classify_frame` already rejected a response carrying
                        // both or neither of result/error, and preserved
                        // whether `result` was present. An absent error plus a
                        // present result is success, including explicit null.
                        if let Some(error) = response.error {
                            return Err(anyhow!("{context} returned an error: {error}"));
                        }
                        if !has_result {
                            bail!("{context} response had no `result` member");
                        }
                        return Ok(Some(response.result.unwrap_or(Value::Null)));
                    }
                    // A response nobody is waiting for: keep it, do not drop it.
                    self.events.push(serde_json::to_value(&response)?);
                    Ok(None)
                }
                JsonRpcMessage::Notification(notification) => {
                    self.events.push(serde_json::to_value(&notification)?);
                    Ok(None)
                }
                JsonRpcMessage::Request(request) => {
                    self.answer(&request)?;
                    self.events.push(serde_json::to_value(&request)?);
                    Ok(None)
                }
            },
        }
    }

    /// Answer a server-initiated request. The spike is read-only: the one
    /// request that could mutate, `workspace/applyEdit`, is declined.
    fn answer(&mut self, request: &JsonRpcRequest) -> Result<()> {
        let outcome: Result<Value, JsonRpcError> = match request.method.as_str() {
            // One entry per requested item; null means "no opinion", which
            // hands the server its own defaults.
            "workspace/configuration" => {
                let items = request
                    .params
                    .as_ref()
                    .and_then(|p| p.get("items"))
                    .and_then(Value::as_array)
                    .map(Vec::len)
                    .unwrap_or(0);
                Ok(Value::Array(vec![Value::Null; items]))
            }
            "window/workDoneProgress/create" => Ok(Value::Null),
            // The client promised support by advertising
            // `workspace.workspaceFolders`; answer with the folders offered at
            // initialize (an array), not null or MethodNotFound.
            "workspace/workspaceFolders" => Ok(self.workspace_folders.clone()),
            "client/registerCapability" | "client/unregisterCapability" => Ok(Value::Null),
            "workspace/applyEdit" => Ok(json!({ "applied": false })),
            other => Err(JsonRpcError::method_not_found(other)),
        };

        let response = match outcome {
            Ok(value) => JsonRpcResponse::success(request.id.clone(), value),
            Err(error) => JsonRpcResponse::error(request.id.clone(), error),
        };
        self.send(&JsonRpcMessage::Response(response))
    }

    fn cleanup(&mut self) {
        // Dropping stdin signals EOF to the server; kill is the backstop.
        let _ = self.stdin.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(handle) = self.reader.take() {
            // The reader can be stranded if a *grandchild* inherited stdout and
            // keeps the pipe open after the direct child is reaped. Poll for
            // completion briefly; if it never comes, detach the thread rather
            // than hang cleanup forever. A stranded thread holds only the
            // closed process's stdout, and the process is exiting anyway.
            let deadline = Instant::now() + Duration::from_secs(2);
            while !self.reader_done.load(Ordering::SeqCst) && Instant::now() < deadline {
                thread::sleep(Duration::from_millis(10));
            }
            if self.reader_done.load(Ordering::SeqCst) {
                let _ = handle.join();
            }
        }
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        self.cleanup();
    }
}

fn reader_loop(mut reader: BufReader<impl Read>, tx: Sender<Incoming>) {
    loop {
        match read_frame(&mut reader) {
            Ok(Some(bytes)) => match classify_frame(&bytes) {
                Ok((message, has_result)) => {
                    if tx
                        .send(Incoming::Message(Box::new(message), has_result))
                        .is_err()
                    {
                        return;
                    }
                }
                // Frame boundary intact; report and keep reading.
                Err(error) => {
                    let detail =
                        format!("{error}; frame bytes: {}", String::from_utf8_lossy(&bytes));
                    if tx.send(Incoming::Malformed(detail)).is_err() {
                        return;
                    }
                }
            },
            Ok(None) => {
                let _ = tx.send(Incoming::Eof);
                return;
            }
            Err(error) => {
                let _ = tx.send(Incoming::Malformed(format!("framing error: {error}")));
                let _ = tx.send(Incoming::Eof);
                return;
            }
        }
    }
}

/// Decode one frame, validating the JSON-RPC response shape *before* serde
/// collapses an explicit `null` to `None`.
///
/// Returns the message and whether the raw object carried a `result` member. A
/// response must carry exactly one of `result` / `error`; neither (or both) is
/// malformed. This keeps a missing `result` from being silently read as a
/// successful null answer.
fn classify_frame(bytes: &[u8]) -> Result<(JsonRpcMessage, bool), String> {
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

/// Read one LSP frame: an ASCII header block ending in a blank line, holding
/// `Content-Length`, followed by exactly that many bytes of JSON. `Ok(None)`
/// is a clean EOF at a frame boundary.
fn read_frame<R: BufRead>(reader: &mut R) -> Result<Option<Vec<u8>>> {
    let headers = match read_headers(reader)? {
        Some(headers) => headers,
        None => return Ok(None),
    };

    let mut content_length: Option<usize> = None;
    let mut header_count = 0usize;
    for line in headers.split(['\r', '\n']) {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        header_count += 1;
        if header_count > MAX_HEADERS {
            bail!("header block has more than {MAX_HEADERS} headers");
        }
        if let Some(rest) = line.strip_prefix("Content-Length:") {
            let value = rest.trim();
            content_length = Some(
                value
                    .parse()
                    .with_context(|| format!("bad Content-Length value `{value}`"))?,
            );
        }
        // Other headers (Content-Type) are legal and ignored.
    }

    let length = content_length.ok_or_else(|| anyhow!("header block had no Content-Length"))?;
    if length > MAX_FRAME_BYTES {
        bail!("Content-Length {length} exceeds the {MAX_FRAME_BYTES}-byte cap");
    }
    let mut body = vec![0u8; length];
    reader
        .read_exact(&mut body)
        .with_context(|| format!("short body: expected {length} bytes"))?;
    Ok(Some(body))
}

/// Read the ASCII header block up to the blank line, one byte at a time so the
/// cap is enforced *before* allocating. `Ok(None)` is a clean EOF at a frame
/// boundary.
fn read_headers<R: BufRead>(reader: &mut R) -> Result<Option<String>> {
    let mut bytes = Vec::new();
    let mut byte = [0u8; 1];
    loop {
        match reader.read_exact(&mut byte) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                if bytes.is_empty() {
                    return Ok(None);
                }
                bail!("EOF in the middle of a header block");
            }
            Err(error) => return Err(error.into()),
        }
        bytes.push(byte[0]);
        if bytes.len() > MAX_HEADER_BYTES {
            bail!("header block exceeds the {MAX_HEADER_BYTES}-byte cap");
        }
        if bytes.ends_with(b"\r\n\r\n") || bytes.ends_with(b"\n\n") {
            break;
        }
    }
    String::from_utf8(bytes)
        .map(Some)
        .context("header block was not valid UTF-8")
}

/// Write one framed message.
fn write_frame<W: Write>(writer: &mut W, body: &[u8]) -> Result<()> {
    write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
    writer.write_all(body)?;
    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn payload() -> &'static [u8] {
        br#"{"jsonrpc":"2.0","id":1,"result":null}"#
    }

    #[test]
    fn frame_round_trips() {
        let mut encoded = Vec::new();
        write_frame(&mut encoded, payload()).unwrap();
        let decoded = read_frame(&mut Cursor::new(encoded)).unwrap().unwrap();
        assert_eq!(decoded, payload());
    }

    #[test]
    fn frame_tolerates_extra_headers_and_crlf() {
        let body = payload();
        let mut encoded = format!(
            "Content-Length: {}\r\nContent-Type: application/json\r\n\r\n",
            body.len()
        )
        .into_bytes();
        encoded.extend_from_slice(body);
        let decoded = read_frame(&mut Cursor::new(encoded)).unwrap().unwrap();
        assert_eq!(decoded, body);
    }

    #[test]
    fn missing_content_length_is_an_error() {
        let encoded = b"Content-Type: application/json\r\n\r\n{}".to_vec();
        assert!(read_frame(&mut Cursor::new(encoded)).is_err());
    }

    #[test]
    fn oversize_length_is_rejected_without_allocating() {
        let encoded = format!("Content-Length: {}\r\n\r\n", MAX_FRAME_BYTES + 1).into_bytes();
        let error = read_frame(&mut Cursor::new(encoded)).unwrap_err();
        assert!(error.to_string().contains("exceeds"));
    }

    #[test]
    fn clean_eof_at_boundary_is_none() {
        assert!(read_frame(&mut Cursor::new(Vec::new())).unwrap().is_none());
    }

    #[test]
    fn eof_inside_a_body_is_an_error() {
        let encoded = b"Content-Length: 10\r\n\r\nshort".to_vec();
        assert!(read_frame(&mut Cursor::new(encoded)).is_err());
    }

    #[test]
    fn successful_null_reply_serializes_result_null() {
        // The reply to window/workDoneProgress/create is success(id, Null).
        // It must carry `"result":null`, never omit `result`.
        let message =
            JsonRpcMessage::Response(JsonRpcResponse::success(RequestId::Integer(7), Value::Null));
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
                assert_eq!(response.result.unwrap_or(Value::Null), Value::Null);
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

    fn python_available() -> bool {
        Command::new("python3")
            .arg("--version")
            .output()
            .map(|output| output.status.success())
            .unwrap_or(false)
    }

    fn echo_for(events: &[Value], id: i64) -> Option<Value> {
        events
            .iter()
            .find(|event| {
                event.get("method").and_then(Value::as_str) == Some("test/response-echo")
                    && event.pointer("/params/id").and_then(Value::as_i64) == Some(id)
            })
            .cloned()
    }

    /// Drive the client against a scripted Python server: proves framing,
    /// request/response correlation, the *content* of the client's replies to
    /// server-initiated requests, unknown-method handling, event collection,
    /// and that a late response after a timeout is preserved rather than lost.
    /// Skipped when `python3` is unavailable.
    #[test]
    fn mock_server_round_trip() {
        if !python_available() {
            eprintln!("skipping mock_server_round_trip: python3 not available");
            return;
        }

        let script = r#"
import json, sys, time

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
    if length is None:
        return None
    return json.loads(sys.stdin.buffer.read(length))

def send(obj):
    data = json.dumps(obj).encode()
    sys.stdout.buffer.write(b"Content-Length: %d\r\n\r\n" % len(data) + data)
    sys.stdout.buffer.flush()

while True:
    msg = read_msg()
    if msg is None:
        break
    if "method" in msg and "id" in msg:
        method = msg["method"]
        if method == "initialize":
            send({"jsonrpc": "2.0", "id": msg["id"], "result": {"capabilities": {}}})
            send({"jsonrpc": "2.0", "method": "test/notice", "params": {"n": 1}})
            # Three server-initiated requests the client must answer.
            send({"jsonrpc": "2.0", "id": 9001, "method": "workspace/configuration",
                  "params": {"items": [{}, {}]}})
            send({"jsonrpc": "2.0", "id": 9002, "method": "workspace/applyEdit",
                  "params": {"edit": {"changes": {}}}})
            send({"jsonrpc": "2.0", "id": 9003, "method": "test/unknown-method",
                  "params": {}})
            send({"jsonrpc": "2.0", "id": 9004, "method": "window/workDoneProgress/create",
                  "params": {"token": "t"}})
            send({"jsonrpc": "2.0", "id": 9005, "method": "workspace/workspaceFolders",
                  "params": {}})
        elif method == "test/late":
            time.sleep(0.4)
            send({"jsonrpc": "2.0", "id": msg["id"], "result": "late"})
        elif method == "test/nullresult":
            send({"jsonrpc": "2.0", "id": msg["id"], "result": None})
        elif method == "shutdown":
            send({"jsonrpc": "2.0", "id": msg["id"], "result": None})
        else:
            send({"jsonrpc": "2.0", "id": msg["id"], "result": "ok"})
    elif "id" in msg:
        # A response from the client: echo it back so the test can assert it.
        send({"jsonrpc": "2.0", "method": "test/response-echo", "params": msg})
"#;

        let path =
            std::env::temp_dir().join(format!("structural-mock-lsp-{}.py", std::process::id()));
        std::fs::write(&path, script).unwrap();

        let mut client = Client::spawn("python3", &[path.to_string_lossy().into_owned()]).unwrap();
        let result = client
            .request(
                "initialize",
                json!({"workspaceFolders": [{"uri": "file:///tmp/ws", "name": "ws"}]}),
                Duration::from_secs(5),
            )
            .unwrap();
        assert!(result.get("capabilities").is_some());

        client.pump(Duration::from_millis(800)).unwrap();

        // The notification and all three server requests were collected.
        assert!(client.events.iter().any(|e| e["method"] == "test/notice"));
        for method in [
            "workspace/configuration",
            "workspace/applyEdit",
            "test/unknown-method",
            "window/workDoneProgress/create",
            "workspace/workspaceFolders",
        ] {
            assert!(
                client.events.iter().any(|e| e["method"] == method),
                "server request {method} was not recorded"
            );
        }

        // ...and the client's *replies* had the right contents.
        let configuration = echo_for(&client.events, 9001).expect("configuration reply");
        assert_eq!(
            configuration.pointer("/params/result"),
            Some(&json!([Value::Null, Value::Null])),
            "workspace/configuration must answer one null per item"
        );

        let apply_edit = echo_for(&client.events, 9002).expect("applyEdit reply");
        assert_eq!(
            apply_edit.pointer("/params/result"),
            Some(&json!({"applied": false})),
            "workspace/applyEdit must be declined, never applied"
        );

        let unknown = echo_for(&client.events, 9003).expect("unknown-method reply");
        assert_eq!(
            unknown
                .pointer("/params/error/code")
                .and_then(Value::as_i64),
            Some(-32601),
            "unknown server requests must get MethodNotFound"
        );

        // The progress handshake reply must carry an explicit result:null,
        // present (not omitted) — this is the reply pyright waits on.
        let progress = echo_for(&client.events, 9004).expect("progress reply");
        assert_eq!(
            progress.pointer("/params/result"),
            Some(&Value::Null),
            "window/workDoneProgress/create must be answered with result:null"
        );
        assert!(
            progress.pointer("/params/method").is_none(),
            "the reply must not carry a method"
        );

        // workspace/workspaceFolders must be answered with the folders offered
        // at initialize, not null or MethodNotFound.
        let folders = echo_for(&client.events, 9005).expect("workspaceFolders reply");
        assert_eq!(
            folders.pointer("/params/result"),
            Some(&json!([{"uri": "file:///tmp/ws", "name": "ws"}])),
            "workspace/workspaceFolders must return the initialized folders"
        );

        // A timed-out request's response must not be lost: it lands in events.
        let late_error = client
            .request("test/late", json!({}), Duration::from_millis(100))
            .unwrap_err();
        assert!(late_error.to_string().contains("timed out"));
        client.pump(Duration::from_millis(600)).unwrap();
        assert!(
            client
                .events
                .iter()
                .any(|e| e.get("id").and_then(Value::as_i64) == Some(2)
                    && e.get("result") == Some(&json!("late"))),
            "the late response to the timed-out request should be preserved in events"
        );

        // An explicit `"result": null` is success with a null answer, not an
        // error and not a timeout.
        let null_answer = client
            .request("test/nullresult", json!({}), Duration::from_secs(5))
            .unwrap();
        assert_eq!(null_answer, Value::Null);

        client.shutdown().unwrap();
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn header_block_oversize_is_rejected() {
        let encoded = vec![b'x'; MAX_HEADER_BYTES + 1];
        let error = read_frame(&mut Cursor::new(encoded)).unwrap_err();
        assert!(error.to_string().contains("header block exceeds"));
    }

    #[test]
    fn too_many_headers_is_rejected() {
        let mut encoded = Vec::new();
        for index in 0..(MAX_HEADERS + 2) {
            encoded.extend_from_slice(format!("X-{index}: 1\r\n").as_bytes());
        }
        encoded.extend_from_slice(b"\r\n");
        encoded.extend_from_slice(b"{}");
        let error = read_frame(&mut Cursor::new(encoded)).unwrap_err();
        assert!(error.to_string().contains("more than"));
    }
}
