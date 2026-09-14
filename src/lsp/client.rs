//! Blocking stdio LSP client over the framing/classification kernel.
//!
//! The shell from the spike transport — child process, reader thread, `mpsc`
//! channel — with the one generalization the spike never needed: responses are
//! routed **by id** rather than matched against the single call currently
//! waiting. [`Client::send_request`] registers an outstanding id,
//! [`Client::wait_response`] pumps until *that* id's response arrives
//! (stashing responses to other outstanding requests on the way), and
//! [`Client::request`] is send+wait for the common sequential case.
//!
//! Honesty about concurrency: several requests can be outstanding, and this is
//! tested with two answered in reverse order. It is not a claim of concurrent
//! execution — the client is a single-threaded blocking shell, and the MCP
//! slice issues its requests strictly sequentially.

use crate::lsp::framing::{read_frame, write_frame};
use crate::lsp::handlers::{self, Reply};
use crate::lsp::jsonrpc::{self, Incoming};
use anyhow::{Context, Result, anyhow, bail};
use mcplease::types::{
    JsonRpcError, JsonRpcMessage, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse, RequestId,
};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::{BufReader, Read};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// A running language server over stdio, addressed synchronously.
pub struct Client {
    child: Child,
    stdin: Option<ChildStdin>,
    rx: Receiver<Incoming>,
    reader: Option<JoinHandle<()>>,
    reader_done: Arc<AtomicBool>,
    next_id: i64,
    /// Requests sent whose response has not arrived yet, mapped to the method
    /// name for error messages.
    outstanding: HashMap<RequestId, String>,
    /// Responses that arrived while some *other* request was being waited on.
    /// Entries are consumed by the matching [`Client::wait_response`].
    completed: HashMap<RequestId, Result<Value, String>>,
    /// Every notification and server request seen so far, plus stray
    /// responses nobody was waiting for. Capped by `event_cap` when set
    /// (long-running servers); the spike runs uncapped so its manifest can
    /// claim "every notification and server request seen" honestly.
    pub events: Vec<Value>,
    event_cap: Option<usize>,
    /// When enabled, log every frame's shape (incoming) or truncated JSON
    /// (outgoing) to stderr. Diagnostic only; enabled by `STRUCTURAL_WIRE`
    /// or `STRUCTURAL_SPIKE_WIRE`, or set directly for tests.
    debug: bool,
    /// The workspace folders captured from the `initialize` params, so a
    /// server-initiated `workspace/workspaceFolders` request can be answered
    /// with them (advertising `workspace.workspaceFolders` promises this).
    workspace_folders: Value,
    /// Protocol state, deliberately separate from the capped `events` log:
    /// the most recent `experimental/serverStatus` notification
    /// (rust-analyzer's readiness signal), retained on arrival. Readiness
    /// must never be derived by rescanning the event log across cap drains.
    pub latest_server_status: Option<Value>,
}

impl Client {
    /// Spawn `command` with `args` and start reader/writer plumbing. stderr is
    /// inherited so server logs reach the terminal (stdio MCP carries nothing
    /// on stderr, so this does not contaminate the protocol channel).
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
            outstanding: HashMap::new(),
            completed: HashMap::new(),
            events: Vec::new(),
            event_cap: None,
            debug: wire_debug_enabled(),
            workspace_folders: Value::Null,
            latest_server_status: None,
        })
    }

    /// Cap `events` to the most recent `cap` entries (a long-running MCP
    /// server would otherwise grow the log without bound). `None` is uncapped,
    /// which is what the spike uses so its manifest stays complete.
    ///
    /// Honest scope: this bounds the event **log** only. The reader channel is
    /// unbounded, so a wedged server flooding notifications between pumps is
    /// not bounded by this cap; readiness and other protocol state is
    /// deliberately retained in dedicated fields (see
    /// [`Client::latest_server_status`]) so it never depends on log contents.
    pub fn set_event_cap(&mut self, cap: Option<usize>) {
        self.event_cap = cap;
        self.trim_events();
    }

    /// Wire tracing is normally enabled through `STRUCTURAL_WIRE` /
    /// `STRUCTURAL_SPIKE_WIRE` at spawn; this setter exists for tests.
    #[allow(dead_code)]
    pub fn set_debug(&mut self, debug: bool) {
        self.debug = debug;
    }

    /// Send a request without waiting: the id is returned and stays
    /// outstanding until its response is consumed by [`Client::wait_response`].
    pub fn send_request(&mut self, method: &str, params: Value) -> Result<RequestId> {
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
        self.outstanding.insert(id.clone(), method.to_string());
        Ok(id)
    }

    /// Wait for the response to an outstanding request, pumping interleaved
    /// notifications, server requests, and responses to *other* outstanding
    /// requests meanwhile (those are stashed, never dropped or lost).
    pub fn wait_response(&mut self, id: &RequestId, timeout: Duration) -> Result<Value> {
        if let Some(outcome) = self.completed.remove(id) {
            return outcome.map_err(anyhow::Error::msg);
        }
        let Some(method) = self.outstanding.get(id).cloned() else {
            bail!("no outstanding request with id {id:?} to wait for");
        };

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
            let incoming = self.rx.recv_timeout(remaining).map_err(|e| match e {
                RecvTimeoutError::Timeout => {
                    anyhow!(
                        "timed out after {timeout:?} waiting for response to {method} (id {id:?})"
                    )
                }
                RecvTimeoutError::Disconnected => {
                    anyhow!("LSP reader thread stopped unexpectedly while waiting for {method}")
                }
            })?;
            self.dispatch(incoming)?;
            if let Some(outcome) = self.completed.remove(id) {
                return outcome.map_err(anyhow::Error::msg);
            }
        }
    }

    /// Send a request and block until its response arrives. Errors on
    /// timeout, EOF, a reader death, or an error response.
    ///
    /// This is the **one-shot** form: on failure the id is retired, so a
    /// response that arrives later cannot strand unreachable in the completed
    /// stash — it is recorded as a stray in [`Self::events`] instead. The
    /// explicit two-step form ([`Self::send_request`] +
    /// [`Self::wait_response`]) keeps resumable semantics: an id whose wait
    /// timed out stays outstanding and its late answer is retrievable.
    pub fn request(&mut self, method: &str, params: Value, timeout: Duration) -> Result<Value> {
        let id = self.send_request(method, params)?;
        match self.wait_response(&id, timeout) {
            Ok(value) => Ok(value),
            Err(error) => {
                self.outstanding.remove(&id);
                self.completed.remove(&id);
                Err(error)
            }
        }
    }

    /// Send a one-way notification.
    pub fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        let frame = JsonRpcMessage::Notification(JsonRpcNotification::new(method, Some(params)));
        self.send(&frame)?;
        // Outgoing notifications are part of the session's message log (the
        // didOpen/didChange/didClose sequence is exactly what diagnostics and
        // tests need to see, in order). Recorded after a successful send so
        // the log never shows a frame the server did not receive.
        self.record(serde_json::to_value(&frame)?);
        Ok(())
    }

    /// Nonblocking drain of every message already queued on the reader
    /// channel. [`Self::pump`] cannot do this — it waits for new arrivals and
    /// a zero timeout returns without receiving — so a status notification
    /// that arrived while the caller was idle would sit unseen. Readiness
    /// decisions run this first. Returns how many messages were processed.
    pub fn drain_queued(&mut self) -> Result<usize> {
        let mut drained = 0usize;
        loop {
            match self.rx.try_recv() {
                Ok(incoming) => {
                    self.dispatch(incoming)?;
                    drained += 1;
                }
                Err(TryRecvError::Empty) => return Ok(drained),
                Err(TryRecvError::Disconnected) => {
                    bail!("LSP reader thread stopped unexpectedly during drain")
                }
            }
        }
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
                Ok(incoming) => self.dispatch(incoming)?,
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

    /// Route one incoming message. Responses to outstanding requests are
    /// stashed in `completed`; everything else is recorded or answered.
    fn dispatch(&mut self, incoming: Incoming) -> Result<()> {
        if self.debug {
            let summary = match &incoming {
                Incoming::Message(message, _) => match &**message {
                    JsonRpcMessage::Request(request) => format!("request {}", request.method),
                    JsonRpcMessage::Notification(notification) => {
                        format!("notification {}", notification.method)
                    }
                    JsonRpcMessage::Response(response) => format!("response id {:?}", response.id),
                },
                Incoming::Malformed(error) => format!("MALFORMED {error}"),
                Incoming::Eof => "EOF".to_string(),
            };
            eprintln!("[lsp<] {summary}");
        }
        match incoming {
            Incoming::Eof => bail!("language server closed stdout"),
            Incoming::Malformed(err) => {
                self.record(json!({ "unparsable": err }));
            }
            Incoming::Message(message, has_result) => match *message {
                JsonRpcMessage::Response(response) => {
                    match response.id.clone() {
                        Some(id) if self.outstanding.contains_key(&id) => {
                            // Registered: stash the outcome for its waiter.
                            // `response_result` distinguishes an explicit
                            // `"result": null` (a valid empty answer) from a
                            // missing `result`, and surfaces error responses.
                            self.outstanding.remove(&id);
                            let outcome = match jsonrpc::response_result(response, has_result) {
                                Ok(value) => Ok(value),
                                Err(error) => Err(error.to_string()),
                            };
                            self.completed.insert(id, outcome);
                        }
                        // A response nobody is waiting for: keep it, do not
                        // drop it.
                        _ => self.record(serde_json::to_value(&response)?),
                    }
                }
                JsonRpcMessage::Notification(notification) => {
                    if notification.method == "experimental/serverStatus" {
                        // Retain readiness in protocol state, not the log.
                        self.latest_server_status = notification.params.clone();
                    }
                    self.record(serde_json::to_value(&notification)?);
                }
                JsonRpcMessage::Request(request) => {
                    self.answer(&request)?;
                    self.record(serde_json::to_value(&request)?);
                }
            },
        }
        Ok(())
    }

    /// Answer a server-initiated request using the pure policy in
    /// [`crate::lsp::handlers`]; this layer only serializes the decision.
    fn answer(&mut self, request: &JsonRpcRequest) -> Result<()> {
        let reply = handlers::server_request_reply(
            &request.method,
            request.params.as_ref(),
            &self.workspace_folders,
        );
        let response = match reply {
            Reply::Success(value) => JsonRpcResponse::success(request.id.clone(), value),
            Reply::Error(code, message) => JsonRpcResponse::error(
                request.id.clone(),
                JsonRpcError {
                    code,
                    message,
                    data: None,
                },
            ),
        };
        self.send(&JsonRpcMessage::Response(response))
    }

    fn record(&mut self, value: Value) {
        self.events.push(value);
        self.trim_events();
    }

    fn trim_events(&mut self) {
        if let Some(cap) = self.event_cap
            && self.events.len() > cap
        {
            let excess = self.events.len() - cap;
            self.events.drain(0..excess);
        }
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

pub(crate) fn wire_debug_enabled() -> bool {
    ["STRUCTURAL_WIRE", "STRUCTURAL_SPIKE_WIRE"]
        .iter()
        .any(|name| std::env::var(name).is_ok_and(|value| value != "0"))
}

fn reader_loop(mut reader: BufReader<impl Read>, tx: Sender<Incoming>) {
    loop {
        match read_frame(&mut reader) {
            Ok(Some(bytes)) => match jsonrpc::classify_frame(&bytes) {
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

#[cfg(test)]
mod tests;
