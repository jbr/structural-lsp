//! A language server on its own threads, addressed through a blocking handle.
//!
//! This replaces the pump-driven shell in [`super::client`]: nothing here
//! waits for a tool call to make progress. Three threads per server:
//!
//! - the **reader** turns the child's stdout into frames and pushes them,
//!   blocking, into the owner's inbox — backpressure lands on the child, never
//!   on a caller;
//! - the **owner** holds every piece of protocol state (pending requests,
//!   open progress tokens, the readiness policy, the event log), answers
//!   server-initiated requests, and is the only thread that touches any of
//!   it;
//! - the **writer** owns the child's stdin, so wire order is enqueue order and
//!   a server that stops reading blocks only this thread, never the owner.
//!
//! Callers hold a [`ServerHandle`] and block on [`ServerHandle::request`],
//! [`ServerHandle::notify`] and [`ServerHandle::wait_ready`]. Every blocking
//! call is bounded by exactly one timer: the **inactivity backstop**. It
//! starts when the operation is admitted, is reset by every message the
//! server sends (progress reports included), and trips only when the server
//! has said nothing for that long. A busy server is not a broken one.
//!
//! # Bounds
//!
//! Every queue is bounded and admission never blocks a caller: the owner's
//! inbox, the writer queue, the number of admitted-but-unfinished
//! operations, the number of open progress tokens and the diagnostic event
//! log all have caps in [`Settings`]. The caps count messages, frames,
//! operations and tokens — not bytes; a single frame is bounded only by
//! [`super::framing::MAX_FRAME_BYTES`] on the way in and by the caller on
//! the way out. A caller whose operation cannot be queued gets an error
//! naming the queue. The one place a full queue is fatal is a *required*
//! frame — the reply to a server-initiated request — which is never dropped
//! silently: the connection is terminated instead.
//!
//! # Teardown
//!
//! [`ServerHandle::shutdown`] and `Drop` both set a terminate flag the owner
//! checks on every wake *and* try to queue a graceful shutdown command; the
//! flag is the reliable path when the inbox is full, the command the fast
//! one. When idle the owner waits at most [`CONTROL_TICK`] between checks,
//! so the flag is always noticed. Both paths converge: a bounded graceful
//! `shutdown`/`exit` attempt, then kill and reap the child **before**
//! waiting for the writer (a write blocked on a wedged server unblocks only
//! when the pipe closes), then bounded joins. Every waiter still registered
//! is failed with the reason; none is left hanging.

use crate::lsp::adapter::{ReadinessPolicy, StandardIdle};
use crate::lsp::client::wire_debug_enabled;
use crate::lsp::framing::{read_frame, write_frame};
use crate::lsp::handlers::{self, Reply};
use crate::lsp::jsonrpc::{self, Incoming};
use anyhow::{Context, Result, anyhow, bail};
use mcplease::types::{
    JsonRpcError, JsonRpcMessage, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse, RequestId,
};
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::BufReader;
use std::process::{Child, ChildStdin, ChildStdout, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender, TrySendError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

/// The longest the owner waits for a message before re-checking the
/// terminate flag and overdue operations. This is a control-plane wake-up,
/// not a settle delay: nothing is processed on a tick that carried no
/// message, and readiness waits complete on the message that satisfies them,
/// not on a tick.
pub const CONTROL_TICK: Duration = Duration::from_millis(250);

/// How long teardown polls a worker thread before detaching it. A reader
/// can be stranded by a grandchild that inherited the server's stdout; the
/// process is exiting anyway and the thread holds nothing but a dead pipe.
const JOIN_GUARD: Duration = Duration::from_secs(2);

/// Capacities and timers for one server connection. Every field bounds a
/// count, not a size; see the module docs.
#[derive(Debug, Clone)]
pub struct Settings {
    /// Messages queued for the owner (server frames and caller commands
    /// share it). The reader blocks when it is full; callers are refused.
    pub inbound_cap: usize,
    /// Frames queued for the writer thread.
    pub writer_cap: usize,
    /// Admitted operations (requests and readiness waits) not yet finished.
    pub pending_cap: usize,
    /// Progress tokens open at once. A server past this is treated as broken.
    pub progress_token_cap: usize,
    /// Diagnostic event log entries retained; oldest are dropped first.
    pub event_cap: usize,
    /// The inactivity backstop for requests and notifications.
    pub backstop: Duration,
    /// How long a graceful `shutdown`/`exit` may take before the child is
    /// killed regardless.
    pub shutdown_grace: Duration,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            inbound_cap: 1024,
            writer_cap: 64,
            pending_cap: 256,
            progress_token_cap: 4096,
            event_cap: 2000,
            backstop: Duration::from_secs(90),
            shutdown_grace: Duration::from_secs(5),
        }
    }
}

/// Routine metadata about a connection. Deliberately does not carry the
/// event log; [`ServerHandle::capture_events`] is the diagnostic path.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub label: String,
    /// `no open progress tokens && policy.is_ready()`.
    pub ready: bool,
    pub open_progress_tokens: usize,
    pub pending_operations: usize,
    /// A one-line summary of the last message the server sent.
    pub last_message: Option<String>,
    pub since_last_message: Option<Duration>,
    /// Set once the connection has failed; every later operation is refused
    /// with this reason.
    pub failure: Option<String>,
}

/// The caller's side of one server connection. Cheap to share across
/// threads by reference; every method takes `&self`.
pub struct ServerHandle {
    label: String,
    inbox: SyncSender<OwnerMsg>,
    inbound_cap: usize,
    terminate: Arc<AtomicBool>,
    owner: Option<JoinHandle<()>>,
    /// How long `shutdown`/`Drop` wait for the owner: the graceful window
    /// plus the teardown's own join guards, with margin.
    join_bound: Duration,
}

impl ServerHandle {
    /// Spawn `command` with default [`Settings`] and the [`StandardIdle`]
    /// policy. stderr is inherited so server logs reach the terminal.
    pub fn spawn(command: &str, args: &[String]) -> Result<Self> {
        Self::spawn_with(command, args, Settings::default(), Box::new(StandardIdle))
    }

    /// Spawn with explicit settings and readiness policy. This is the
    /// injection point for tests and for per-server adapters.
    pub fn spawn_with(
        command: &str,
        args: &[String],
        settings: Settings,
        policy: Box<dyn ReadinessPolicy>,
    ) -> Result<Self> {
        let label = std::iter::once(command)
            .chain(args.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join(" ");
        let mut child = std::process::Command::new(command)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .with_context(|| format!("spawning `{label}`"))?;
        let stdin = child.stdin.take().context("child stdin was not piped")?;
        let stdout = child.stdout.take().context("child stdout was not piped")?;

        let (inbox_tx, inbox_rx) = mpsc::sync_channel::<OwnerMsg>(settings.inbound_cap);
        let (writer_tx, writer_rx) = mpsc::sync_channel::<WriterJob>(settings.writer_cap);
        let terminate = Arc::new(AtomicBool::new(false));
        let writer_terminating = Arc::new(AtomicBool::new(false));

        let spawned = (|| -> Result<(JoinHandle<()>, JoinHandle<()>)> {
            let reader = thread::Builder::new()
                .name("lsp-reader".into())
                .spawn({
                    let inbox = inbox_tx.clone();
                    move || reader_loop(BufReader::new(stdout), inbox)
                })
                .context("spawning reader thread")?;
            let writer = thread::Builder::new()
                .name("lsp-writer".into())
                .spawn({
                    let inbox = inbox_tx.clone();
                    let terminating = Arc::clone(&writer_terminating);
                    move || writer_loop(stdin, writer_rx, terminating, inbox)
                })
                .context("spawning writer thread")?;
            Ok((reader, writer))
        })();
        let (reader, writer) = match spawned {
            Ok(threads) => threads,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        };

        let join_bound = settings.shutdown_grace + 2 * JOIN_GUARD + Duration::from_secs(1);
        let inbound_cap = settings.inbound_cap;
        let progress = ProgressTokens::new(settings.progress_token_cap);
        let owner = Owner {
            label: label.clone(),
            settings,
            policy,
            child,
            inbox: inbox_rx,
            writer_tx: Some(writer_tx),
            writer: Some(writer),
            reader: Some(reader),
            terminate: Arc::clone(&terminate),
            writer_terminating,
            next_id: 1,
            pending: HashMap::new(),
            waiters: Vec::new(),
            progress,
            events: VecDeque::new(),
            last_incoming: None,
            last_summary: None,
            workspace_folders: Value::Null,
            failed: None,
            shutting_down: None,
            shutdown_acknowledged: false,
            debug: wire_debug_enabled(),
        };
        let owner = thread::Builder::new()
            .name("lsp-owner".into())
            .spawn(move || owner.run())
            .context("spawning owner thread")?;

        Ok(Self {
            label,
            inbound_cap,
            inbox: inbox_tx,
            terminate,
            owner: Some(owner),
            join_bound,
        })
    }

    /// `command args…`, for error messages.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Send a request and block until its response arrives, the connection
    /// fails, or the inactivity backstop trips. An error response is an
    /// `Err`; an explicit `"result": null` is `Ok(Value::Null)`.
    pub fn request(&self, method: &str, params: Value) -> Result<Value> {
        let (reply, outcome) = mpsc::channel();
        self.admit(Command::Request {
            method: method.to_string(),
            params,
            reply,
            admitted_at: Instant::now(),
        })?;
        match outcome.recv() {
            Ok(result) => result,
            Err(_) => bail!(
                "connection to {} closed before a response to {method} arrived",
                self.label
            ),
        }
    }

    /// Send a notification and block until the writer has written it. `Ok`
    /// means the bytes reached the server's stdin; it is never claimed
    /// earlier than that.
    pub fn notify(&self, method: &str, params: Value) -> Result<()> {
        let (ack, outcome) = mpsc::channel();
        self.admit(Command::Notify {
            method: method.to_string(),
            params,
            ack,
            admitted_at: Instant::now(),
        })?;
        match outcome.recv() {
            Ok(result) => result,
            Err(_) => bail!(
                "connection to {} closed before {method} was written",
                self.label
            ),
        }
    }

    /// Block until the server has no outstanding work: no open progress
    /// token and the readiness policy agrees. Returns immediately when that
    /// already holds. Errors when the connection fails or the server sends
    /// nothing for `backstop`.
    pub fn wait_ready(&self, backstop: Duration) -> Result<()> {
        let (reply, outcome) = mpsc::channel();
        self.admit(Command::WaitReady {
            backstop,
            reply,
            admitted_at: Instant::now(),
        })?;
        match outcome.recv() {
            Ok(result) => result,
            Err(_) => bail!("connection to {} closed while waiting for readiness", self.label),
        }
    }

    /// Routine metadata; never clones the event log.
    pub fn snapshot(&self) -> Result<Snapshot> {
        let (reply, outcome) = mpsc::channel();
        self.admit(Command::Snapshot { reply })?;
        outcome
            .recv()
            .map_err(|_| anyhow!("connection to {} closed", self.label))
    }

    /// The diagnostic event log: every notification, server request, stray
    /// response and outgoing frame, oldest first, up to `event_cap`.
    pub fn capture_events(&self) -> Result<Vec<Value>> {
        let (reply, outcome) = mpsc::channel();
        self.admit(Command::CaptureEvents { reply })?;
        outcome
            .recv()
            .map_err(|_| anyhow!("connection to {} closed", self.label))
    }

    /// Shut the server down: a graceful `shutdown`/`exit` bounded by
    /// `shutdown_grace`, then kill and reap. Returns once the owner has torn
    /// down or the join bound expires (the owner finishes on its own then).
    pub fn shutdown(mut self) -> Result<()> {
        self.begin_teardown();
        Ok(())
    }

    fn admit(&self, command: Command) -> Result<()> {
        self.inbox
            .try_send(OwnerMsg::Admit(command))
            .map_err(|error| match error {
                TrySendError::Full(_) => anyhow!(
                    "{}: local inbound queue full ({} messages); operation not admitted",
                    self.label,
                    self.inbound_cap
                ),
                TrySendError::Disconnected(_) => anyhow!("connection to {} closed", self.label),
            })
    }

    fn begin_teardown(&mut self) {
        // Already done, or a handle that never owned the connection.
        let Some(owner) = self.owner.take() else {
            return;
        };
        self.terminate.store(true, Ordering::SeqCst);
        // Fast path; the flag above is the path that cannot fail.
        let _ = self.inbox.try_send(OwnerMsg::Admit(Command::Shutdown));
        bounded_join(owner, self.join_bound);
    }
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        self.begin_teardown();
    }
}

// --- messages between the threads ---

enum OwnerMsg {
    FromServer(Incoming),
    Admit(Command),
    /// The writer could not write a frame; the connection is dead.
    WriteFailed(String),
}

enum Command {
    Request {
        method: String,
        params: Value,
        reply: Sender<Result<Value>>,
        admitted_at: Instant,
    },
    Notify {
        method: String,
        params: Value,
        ack: Sender<Result<()>>,
        admitted_at: Instant,
    },
    WaitReady {
        backstop: Duration,
        reply: Sender<Result<()>>,
        admitted_at: Instant,
    },
    Snapshot {
        reply: Sender<Snapshot>,
    },
    CaptureEvents {
        reply: Sender<Vec<Value>>,
    },
    Shutdown,
}

struct WriterJob {
    frame: Vec<u8>,
    /// Present for notifications: the caller is told the real outcome of
    /// the write. Requests carry none — their outcome is the response.
    ack: Option<Sender<Result<()>>>,
}

struct PendingRequest {
    method: String,
    /// `None` for the owner's own `shutdown` request.
    reply: Option<Sender<Result<Value>>>,
    admitted_at: Instant,
    backstop: Duration,
}

struct Waiter {
    reply: Sender<Result<()>>,
    admitted_at: Instant,
    backstop: Duration,
}

// --- progress tracking ---

/// The set of progress tokens the server has opened and not ended.
/// `window/workDoneProgress/create` and a `$/progress` `begin` both open a
/// token; `end` closes it; `report` only proves the server is alive.
struct ProgressTokens {
    open: HashSet<String>,
    cap: usize,
}

impl ProgressTokens {
    fn new(cap: usize) -> Self {
        Self {
            open: HashSet::new(),
            cap,
        }
    }

    fn is_idle(&self) -> bool {
        self.open.is_empty()
    }

    fn len(&self) -> usize {
        self.open.len()
    }

    /// Fold one server-initiated request or notification into the set.
    /// `Err` means the server exceeded the token cap.
    fn observe(&mut self, method: &str, params: Option<&Value>) -> Result<(), String> {
        let token = params.and_then(|p| p.get("token")).map(canonical_token);
        match method {
            "window/workDoneProgress/create" => match token {
                Some(token) => self.open_token(token),
                None => Ok(()),
            },
            "$/progress" => {
                let kind = params
                    .and_then(|p| p.pointer("/value/kind"))
                    .and_then(Value::as_str);
                match (kind, token) {
                    (Some("begin"), Some(token)) => self.open_token(token),
                    (Some("end"), Some(token)) => {
                        self.open.remove(&token);
                        Ok(())
                    }
                    _ => Ok(()),
                }
            }
            _ => Ok(()),
        }
    }

    fn open_token(&mut self, token: String) -> Result<(), String> {
        if !self.open.contains(&token) && self.open.len() >= self.cap {
            return Err(format!(
                "server opened more than {} progress tokens at once",
                self.cap
            ));
        }
        self.open.insert(token);
        Ok(())
    }
}

/// Tokens are strings or integers on the wire; key both the same way.
fn canonical_token(token: &Value) -> String {
    match token {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

// --- the owner thread ---

struct Owner {
    label: String,
    settings: Settings,
    policy: Box<dyn ReadinessPolicy>,
    child: Child,
    inbox: Receiver<OwnerMsg>,
    writer_tx: Option<SyncSender<WriterJob>>,
    writer: Option<JoinHandle<()>>,
    reader: Option<JoinHandle<()>>,
    terminate: Arc<AtomicBool>,
    writer_terminating: Arc<AtomicBool>,
    next_id: i64,
    pending: HashMap<RequestId, PendingRequest>,
    waiters: Vec<Waiter>,
    progress: ProgressTokens,
    events: VecDeque<Value>,
    last_incoming: Option<Instant>,
    last_summary: Option<String>,
    workspace_folders: Value,
    /// Set by the first fatal condition; the loop exits on the next pass.
    failed: Option<String>,
    /// The graceful window's deadline once shutdown has begun.
    shutting_down: Option<Instant>,
    shutdown_acknowledged: bool,
    debug: bool,
}

impl Owner {
    fn run(mut self) {
        loop {
            if self.terminate.load(Ordering::SeqCst) {
                self.begin_shutdown();
            }
            if self.done() {
                break;
            }
            let wait = self
                .next_deadline()
                .map(|deadline| deadline.saturating_duration_since(Instant::now()))
                .map_or(CONTROL_TICK, |until| until.min(CONTROL_TICK));
            match self.inbox.recv_timeout(wait) {
                Ok(message) => self.process(message),
                Err(RecvTimeoutError::Timeout) => {}
                // Every sender is gone: the handle, the reader and the
                // writer. Nothing can arrive; tear down.
                Err(RecvTimeoutError::Disconnected) => break,
            }
            // After every wake, message or not, so continuous traffic
            // cannot starve the backstop.
            self.expire_overdue();
        }
        self.teardown();
        self.serve_refusals();
    }

    /// After teardown the owner lingers, without a child or workers, only to
    /// refuse later operations with the real reason and answer snapshots,
    /// until the handle asks it to go. A caller that finds the connection
    /// failed learns *why*, not merely that it is gone.
    fn serve_refusals(&mut self) {
        loop {
            if self.terminate.load(Ordering::SeqCst) {
                return;
            }
            match self.inbox.recv_timeout(CONTROL_TICK) {
                Ok(OwnerMsg::Admit(Command::Shutdown)) => return,
                Ok(OwnerMsg::Admit(command)) => self.admit(command),
                Ok(OwnerMsg::FromServer(_)) | Ok(OwnerMsg::WriteFailed(_)) => {}
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    }

    fn done(&self) -> bool {
        if self.failed.is_some() {
            return true;
        }
        match self.shutting_down {
            Some(deadline) => self.shutdown_acknowledged || Instant::now() >= deadline,
            None => false,
        }
    }

    fn process(&mut self, message: OwnerMsg) {
        match message {
            OwnerMsg::FromServer(incoming) => self.on_incoming(incoming),
            OwnerMsg::Admit(command) => self.admit(command),
            OwnerMsg::WriteFailed(reason) => self.fail(reason),
        }
    }

    // -- incoming --

    fn on_incoming(&mut self, incoming: Incoming) {
        if self.debug {
            let summary = match &incoming {
                Incoming::Message(message, _) => summarize(message),
                Incoming::Malformed(error) => format!("MALFORMED {error}"),
                Incoming::Eof => "EOF".to_string(),
            };
            eprintln!("[lsp<] {summary}");
        }
        match incoming {
            Incoming::Eof => self.fail("language server closed stdout".to_string()),
            Incoming::Malformed(error) => {
                self.touch("malformed frame".to_string());
                self.record(json!({ "unparsable": error }));
            }
            Incoming::Message(message, has_result) => {
                self.touch(summarize(&message));
                match *message {
                    JsonRpcMessage::Response(response) => self.on_response(response, has_result),
                    JsonRpcMessage::Notification(notification) => {
                        self.on_notification(notification)
                    }
                    JsonRpcMessage::Request(request) => self.on_server_request(request),
                }
            }
        }
        self.complete_ready_waiters();
    }

    /// Any message resets inactivity for every pending operation.
    fn touch(&mut self, summary: String) {
        self.last_incoming = Some(Instant::now());
        self.last_summary = Some(summary);
    }

    fn on_response(&mut self, response: JsonRpcResponse, has_result: bool) {
        let Some(id) = response.id.clone() else {
            self.record(serde_json::to_value(&response).unwrap_or(Value::Null));
            return;
        };
        let Some(pending) = self.pending.remove(&id) else {
            // Nobody is waiting: a retired or unknown id. Keep it, do not
            // drop it.
            self.record(serde_json::to_value(&response).unwrap_or(Value::Null));
            return;
        };
        let outcome = jsonrpc::response_result(response, has_result);
        match pending.reply {
            Some(reply) => {
                let _ = reply.send(outcome);
            }
            None if pending.method == "shutdown" => {
                self.shutdown_acknowledged = true;
                let _ = self.enqueue(
                    &JsonRpcMessage::Notification(JsonRpcNotification::new("exit", None)),
                    None,
                );
            }
            None => {}
        }
    }

    fn on_notification(&mut self, notification: JsonRpcNotification) {
        if let Err(reason) = self
            .progress
            .observe(&notification.method, notification.params.as_ref())
        {
            self.fail(reason);
            return;
        }
        self.policy
            .observe(&notification.method, notification.params.as_ref());
        self.record(serde_json::to_value(&notification).unwrap_or(Value::Null));
    }

    fn on_server_request(&mut self, request: JsonRpcRequest) {
        if let Err(reason) = self
            .progress
            .observe(&request.method, request.params.as_ref())
        {
            self.fail(reason);
            return;
        }
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
        // A required frame: the server is waiting on it. Not queueable means
        // the connection cannot be kept coherent.
        if let Err(reason) = self.enqueue(&JsonRpcMessage::Response(response), None) {
            self.fail(format!(
                "could not queue the reply to server request {}: {reason}",
                request.method
            ));
            return;
        }
        self.record(serde_json::to_value(&request).unwrap_or(Value::Null));
    }

    // -- commands --

    fn admit(&mut self, command: Command) {
        match command {
            Command::Request {
                method,
                params,
                reply,
                admitted_at,
            } => {
                if let Some(reason) = self.refusal() {
                    let _ = reply.send(Err(anyhow!("{reason}")));
                    return;
                }
                if self.operations() >= self.settings.pending_cap {
                    let _ = reply.send(Err(anyhow!(
                        "{}: {} operations already pending (cap); {method} not admitted",
                        self.label,
                        self.settings.pending_cap
                    )));
                    return;
                }
                if method == "initialize" {
                    // A server that sees `workspace.workspaceFolders` may
                    // later ask for these.
                    self.workspace_folders = params
                        .get("workspaceFolders")
                        .cloned()
                        .unwrap_or(Value::Null);
                }
                let id = RequestId::Integer(self.next_id);
                self.next_id += 1;
                let frame = JsonRpcMessage::Request(JsonRpcRequest::new(
                    id.clone(),
                    method.as_str(),
                    Some(params),
                ));
                // Registered before the frame can be written: the owner is
                // the only thread that routes responses, so none can be
                // processed between here and the enqueue below.
                self.pending.insert(
                    id.clone(),
                    PendingRequest {
                        method: method.clone(),
                        reply: Some(reply.clone()),
                        admitted_at,
                        backstop: self.settings.backstop,
                    },
                );
                if let Err(reason) = self.enqueue(&frame, None) {
                    self.pending.remove(&id);
                    let _ = reply.send(Err(anyhow!("{}: {method} not sent: {reason}", self.label)));
                }
            }
            Command::Notify {
                method,
                params,
                ack,
                admitted_at: _,
            } => {
                if let Some(reason) = self.refusal() {
                    let _ = ack.send(Err(anyhow!("{reason}")));
                    return;
                }
                let frame = JsonRpcMessage::Notification(JsonRpcNotification::new(
                    method.as_str(),
                    Some(params),
                ));
                if let Err(reason) = self.enqueue(&frame, Some(ack.clone())) {
                    let _ = ack.send(Err(anyhow!("{}: {method} not sent: {reason}", self.label)));
                }
            }
            Command::WaitReady {
                backstop,
                reply,
                admitted_at,
            } => {
                if let Some(reason) = self.refusal() {
                    let _ = reply.send(Err(anyhow!("{reason}")));
                    return;
                }
                if self.ready() {
                    let _ = reply.send(Ok(()));
                    return;
                }
                if self.operations() >= self.settings.pending_cap {
                    let _ = reply.send(Err(anyhow!(
                        "{}: {} operations already pending (cap); readiness wait not admitted",
                        self.label,
                        self.settings.pending_cap
                    )));
                    return;
                }
                self.waiters.push(Waiter {
                    reply,
                    admitted_at,
                    backstop,
                });
            }
            Command::Snapshot { reply } => {
                let _ = reply.send(self.snapshot());
            }
            Command::CaptureEvents { reply } => {
                let _ = reply.send(self.events.iter().cloned().collect());
            }
            Command::Shutdown => self.begin_shutdown(),
        }
    }

    fn refusal(&self) -> Option<String> {
        if let Some(reason) = &self.failed {
            return Some(format!("connection to {} failed: {reason}", self.label));
        }
        if self.shutting_down.is_some() {
            return Some(format!("connection to {} is shutting down", self.label));
        }
        None
    }

    fn operations(&self) -> usize {
        self.pending.len() + self.waiters.len()
    }

    fn ready(&self) -> bool {
        self.progress.is_idle() && self.policy.is_ready()
    }

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            label: self.label.clone(),
            ready: self.ready(),
            open_progress_tokens: self.progress.len(),
            pending_operations: self.operations(),
            last_message: self.last_summary.clone(),
            since_last_message: self.last_incoming.map(|at| at.elapsed()),
            failure: self.failed.clone(),
        }
    }

    fn complete_ready_waiters(&mut self) {
        if self.waiters.is_empty() || !self.ready() {
            return;
        }
        for waiter in self.waiters.drain(..) {
            let _ = waiter.reply.send(Ok(()));
        }
    }

    // -- outgoing --

    /// Serialize and hand a frame to the writer. `Err` is the reason it could
    /// not be queued; nothing was written.
    fn enqueue(
        &mut self,
        message: &JsonRpcMessage,
        ack: Option<Sender<Result<()>>>,
    ) -> Result<(), String> {
        let frame = serde_json::to_vec(message).map_err(|e| format!("serializing frame: {e}"))?;
        let Some(writer) = &self.writer_tx else {
            return Err("writer already released".to_string());
        };
        match writer.try_send(WriterJob { frame, ack }) {
            Ok(()) => {
                if self.debug {
                    eprintln!("[lsp>] {}", truncated(message));
                }
                // Recorded when handed to the writer, in enqueue order — the
                // order it will reach the wire.
                self.record(serde_json::to_value(message).unwrap_or(Value::Null));
                Ok(())
            }
            Err(TrySendError::Full(_)) => Err(format!(
                "local write queue full ({} frames)",
                self.settings.writer_cap
            )),
            Err(TrySendError::Disconnected(_)) => Err("writer thread gone".to_string()),
        }
    }

    fn record(&mut self, value: Value) {
        self.events.push_back(value);
        while self.events.len() > self.settings.event_cap {
            self.events.pop_front();
        }
    }

    // -- timers --

    /// When an operation admitted at `admitted_at` with this backstop trips:
    /// inactivity is measured from the later of admission and the server's
    /// last message, so a long-idle server gets a fresh budget on a new
    /// operation and any message extends every pending one.
    fn deadline_for(&self, admitted_at: Instant, backstop: Duration) -> Instant {
        let since = match self.last_incoming {
            Some(last) if last > admitted_at => last,
            _ => admitted_at,
        };
        since + backstop
    }

    fn next_deadline(&self) -> Option<Instant> {
        let pending = self
            .pending
            .values()
            .map(|p| self.deadline_for(p.admitted_at, p.backstop));
        let waiters = self
            .waiters
            .iter()
            .map(|w| self.deadline_for(w.admitted_at, w.backstop));
        pending.chain(waiters).chain(self.shutting_down).min()
    }

    fn expire_overdue(&mut self) {
        if self.failed.is_some() {
            return;
        }
        let now = Instant::now();
        let overdue: Vec<RequestId> = self
            .pending
            .iter()
            .filter(|(_, p)| now >= self.deadline_for(p.admitted_at, p.backstop))
            .map(|(id, _)| id.clone())
            .collect();
        for id in overdue {
            let pending = self.pending.remove(&id).expect("collected above");
            let error = self.inactivity_error(&pending.method, pending.admitted_at, pending.backstop);
            if let Some(reply) = pending.reply {
                let _ = reply.send(Err(error));
            }
        }
        let waiters = std::mem::take(&mut self.waiters);
        for waiter in waiters {
            if now >= self.deadline_for(waiter.admitted_at, waiter.backstop) {
                let error =
                    self.inactivity_error("readiness wait", waiter.admitted_at, waiter.backstop);
                let _ = waiter.reply.send(Err(error));
            } else {
                self.waiters.push(waiter);
            }
        }
    }

    fn inactivity_error(
        &self,
        what: &str,
        admitted_at: Instant,
        backstop: Duration,
    ) -> anyhow::Error {
        let since = match self.last_incoming {
            Some(last) if last > admitted_at => last,
            _ => admitted_at,
        };
        anyhow!(
            "no message from {} for {:.1?} (inactivity backstop {:?}) while {what} was pending, admitted {:.1?} ago; last message: {}",
            self.label,
            since.elapsed(),
            backstop,
            admitted_at.elapsed(),
            self.last_summary.as_deref().unwrap_or("none yet")
        )
    }

    // -- lifecycle --

    /// The graceful path: ask the server to shut down, bounded by the grace
    /// window. Idempotent; a no-op once the connection has failed.
    fn begin_shutdown(&mut self) {
        if self.shutting_down.is_some() || self.failed.is_some() {
            return;
        }
        let grace = self.settings.shutdown_grace;
        self.shutting_down = Some(Instant::now() + grace);
        let id = RequestId::Integer(self.next_id);
        self.next_id += 1;
        self.pending.insert(
            id.clone(),
            PendingRequest {
                method: "shutdown".to_string(),
                reply: None,
                admitted_at: Instant::now(),
                backstop: grace,
            },
        );
        let frame = JsonRpcMessage::Request(JsonRpcRequest::new(id.clone(), "shutdown", None));
        if self.enqueue(&frame, None).is_err() {
            // Best effort only; the kill at the end of the window is the
            // guarantee.
            self.pending.remove(&id);
        }
    }

    /// A fatal condition: fail every waiter, kill and reap the child so a
    /// writer blocked on its stdin unblocks, and let the loop exit.
    fn fail(&mut self, reason: String) {
        if self.failed.is_some() {
            return;
        }
        self.failed = Some(reason.clone());
        self.fail_all(&format!("connection to {} failed: {reason}", self.label));
        self.kill_and_reap();
    }

    fn fail_all(&mut self, reason: &str) {
        for (_, pending) in self.pending.drain() {
            if let Some(reply) = pending.reply {
                let _ = reply.send(Err(anyhow!("{reason} ({} was pending)", pending.method)));
            }
        }
        for waiter in self.waiters.drain(..) {
            let _ = waiter.reply.send(Err(anyhow!("{reason} (readiness wait was pending)")));
        }
    }

    fn kill_and_reap(&mut self) {
        // Kill, then close the pipes by reaping: a writer blocked on stdin
        // gets EPIPE, a reader gets EOF.
        let _ = self.child.kill();
        let _ = self.child.wait();
        self.writer_terminating.store(true, Ordering::SeqCst);
    }

    fn teardown(&mut self) {
        if self.failed.is_none() {
            self.fail_all(&format!("connection to {} shut down", self.label));
        }
        self.kill_and_reap();
        // The writer exits when its queue closes (it never blocks on the
        // inbox); the reader when its pipe ends — and its final sends need
        // room in the inbox, so drain while waiting for it.
        drop(self.writer_tx.take());
        if let Some(writer) = self.writer.take() {
            bounded_join(writer, JOIN_GUARD);
        }
        if let Some(reader) = self.reader.take() {
            let deadline = Instant::now() + JOIN_GUARD;
            while !reader.is_finished() && Instant::now() < deadline {
                while self.inbox.try_recv().is_ok() {}
                thread::sleep(Duration::from_millis(10));
            }
            if reader.is_finished() {
                let _ = reader.join();
            }
        }
    }
}

fn summarize(message: &JsonRpcMessage) -> String {
    match message {
        JsonRpcMessage::Request(request) => format!("request {}", request.method),
        JsonRpcMessage::Notification(notification) => {
            format!("notification {}", notification.method)
        }
        JsonRpcMessage::Response(response) => format!("response to id {:?}", response.id),
    }
}

fn truncated(message: &JsonRpcMessage) -> String {
    let text = serde_json::to_string(message).unwrap_or_default();
    if text.chars().count() > 240 {
        format!("{}…", text.chars().take(240).collect::<String>())
    } else {
        text
    }
}

/// Wait for a worker to finish, polling briefly; detach it if it does not.
/// A detached worker holds only a dead pipe or a closed channel.
fn bounded_join(handle: JoinHandle<()>, bound: Duration) {
    let deadline = Instant::now() + bound;
    while !handle.is_finished() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    if handle.is_finished() {
        let _ = handle.join();
    }
}

// --- the worker threads ---

/// Frames from the server into the owner's inbox. The send blocks when the
/// inbox is full: that is backpressure on the child, by design.
fn reader_loop(mut reader: BufReader<ChildStdout>, inbox: SyncSender<OwnerMsg>) {
    loop {
        let incoming = match read_frame(&mut reader) {
            Ok(Some(bytes)) => match jsonrpc::classify_frame(&bytes) {
                Ok((message, has_result)) => Incoming::Message(Box::new(message), has_result),
                // Frame boundary intact: report it and keep reading.
                Err(error) => Incoming::Malformed(format!(
                    "{error}; frame bytes: {}",
                    String::from_utf8_lossy(&bytes)
                )),
            },
            Ok(None) => {
                let _ = inbox.send(OwnerMsg::FromServer(Incoming::Eof));
                return;
            }
            Err(error) => {
                let _ = inbox.send(OwnerMsg::FromServer(Incoming::Malformed(format!(
                    "framing error: {error}"
                ))));
                let _ = inbox.send(OwnerMsg::FromServer(Incoming::Eof));
                return;
            }
        };
        if inbox.send(OwnerMsg::FromServer(incoming)).is_err() {
            return;
        }
    }
}

/// Frames from the owner onto the server's stdin, acknowledging each one
/// with its real outcome. After the first failed write, or once the owner
/// is terminating, every remaining job is acknowledged as not written
/// rather than left unanswered. Exits when the owner drops the queue.
fn writer_loop(
    mut stdin: ChildStdin,
    jobs: Receiver<WriterJob>,
    terminating: Arc<AtomicBool>,
    inbox: SyncSender<OwnerMsg>,
) {
    let mut broken: Option<String> = None;
    for job in jobs {
        if let Some(reason) = &broken {
            acknowledge(job.ack, Err(anyhow!("frame not written: {reason}")));
            continue;
        }
        if terminating.load(Ordering::SeqCst) {
            acknowledge(
                job.ack,
                Err(anyhow!("connection terminated before the frame was written")),
            );
            continue;
        }
        match write_frame(&mut stdin, &job.frame) {
            Ok(()) => acknowledge(job.ack, Ok(())),
            Err(error) => {
                let reason = format!("writing to the server failed: {error}");
                acknowledge(job.ack, Err(anyhow!("{reason}")));
                // Nonblocking: if the inbox is full the owner is draining it
                // and will learn of the death from the reader's EOF instead.
                let _ = inbox.try_send(OwnerMsg::WriteFailed(reason.clone()));
                broken = Some(reason);
            }
        }
    }
}

fn acknowledge(ack: Option<Sender<Result<()>>>, outcome: Result<()>) {
    if let Some(ack) = ack {
        let _ = ack.send(outcome);
    }
}

#[cfg(test)]
mod progress_tests {
    use super::ProgressTokens;
    use serde_json::json;

    #[test]
    fn create_begin_and_end_track_the_open_set() {
        let mut tokens = ProgressTokens::new(8);
        assert!(tokens.is_idle());
        let create = json!({ "token": "a" });
        tokens
            .observe("window/workDoneProgress/create", Some(&create))
            .unwrap();
        assert_eq!(tokens.len(), 1, "create opens the token");
        let begin = json!({ "token": "a", "value": { "kind": "begin", "title": "x" } });
        tokens.observe("$/progress", Some(&begin)).unwrap();
        assert_eq!(tokens.len(), 1, "begin on a created token is not a second token");
        let report = json!({ "token": "a", "value": { "kind": "report" } });
        tokens.observe("$/progress", Some(&report)).unwrap();
        assert!(!tokens.is_idle(), "report keeps it open");
        let end = json!({ "token": "a", "value": { "kind": "end" } });
        tokens.observe("$/progress", Some(&end)).unwrap();
        assert!(tokens.is_idle(), "end closes it");
        // Integer and string tokens key the same way.
        let begin = json!({ "token": 7, "value": { "kind": "begin" } });
        tokens.observe("$/progress", Some(&begin)).unwrap();
        let end = json!({ "token": "7", "value": { "kind": "end" } });
        tokens.observe("$/progress", Some(&end)).unwrap();
        assert!(tokens.is_idle());
    }

    #[test]
    fn exceeding_the_token_cap_is_an_error() {
        let mut tokens = ProgressTokens::new(2);
        for token in ["a", "b"] {
            let begin = json!({ "token": token, "value": { "kind": "begin" } });
            tokens.observe("$/progress", Some(&begin)).unwrap();
        }
        let begin = json!({ "token": "c", "value": { "kind": "begin" } });
        let error = tokens.observe("$/progress", Some(&begin)).unwrap_err();
        assert!(error.contains("more than 2"), "{error}");
        // Re-opening an already open token is not a new token.
        let begin = json!({ "token": "a", "value": { "kind": "begin" } });
        tokens.observe("$/progress", Some(&begin)).unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    /// A Python interpreter that starts quickly. `python3` on PATH may be a
    /// version-manager shim that takes over a second to resolve, which
    /// would eat the short backstops some tests use; prefer a direct
    /// binary, and let `STRUCTURAL_TEST_PYTHON` name one explicitly.
    fn python() -> Option<String> {
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
                std::process::Command::new(candidate)
                    .arg("--version")
                    .output()
                    .map(|output| output.status.success())
                    .unwrap_or(false)
            })
            .map(str::to_string)
    }

    /// Small capacities so overload paths are reachable; a long backstop so
    /// only the tests that set their own can trip it.
    fn test_settings() -> Settings {
        Settings {
            inbound_cap: 64,
            writer_cap: 8,
            pending_cap: 8,
            progress_token_cap: 8,
            event_cap: 200,
            backstop: Duration::from_secs(5),
            shutdown_grace: Duration::from_millis(500),
        }
    }

    struct Mock {
        python: String,
        script: std::path::PathBuf,
    }

    impl Mock {
        fn new(name: &str) -> Self {
            let script = std::env::temp_dir().join(format!(
                "structural-handle-mock-{name}-{}.py",
                std::process::id()
            ));
            std::fs::write(&script, MOCK_PY).unwrap();
            Self {
                python: python().expect("checked by needs_python!"),
                script,
            }
        }

        fn spawn(&self, settings: Settings, policy: Box<dyn ReadinessPolicy>) -> ServerHandle {
            let args = vec![self.script.to_string_lossy().into_owned()];
            ServerHandle::spawn_with(&self.python, &args, settings, policy).unwrap()
        }
    }

    impl Drop for Mock {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.script);
        }
    }

    /// A readiness policy driven by `test/gate` notifications.
    struct Gate(Arc<AtomicBool>);

    impl ReadinessPolicy for Gate {
        fn observe(&mut self, method: &str, params: Option<&Value>) {
            if method == "test/gate" {
                let open = params
                    .and_then(|p| p.get("open"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                self.0.store(open, Ordering::SeqCst);
            }
        }
        fn is_ready(&self) -> bool {
            self.0.load(Ordering::SeqCst)
        }
    }

    fn initialize(handle: &ServerHandle) {
        let result = handle
            .request(
                "initialize",
                json!({"workspaceFolders": [{"uri": "file:///tmp/ws", "name": "ws"}]}),
            )
            .unwrap();
        assert!(result.get("capabilities").is_some());
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

    macro_rules! needs_python {
        () => {
            if python().is_none() {
                eprintln!("skipping: python3 not available");
                return;
            }
        };
    }

    #[test]
    fn concurrent_requests_correlate_in_reverse_order() {
        needs_python!();
        let mock = Mock::new("pair");
        let handle = mock.spawn(test_settings(), Box::new(StandardIdle));
        initialize(&handle);

        // Whichever `test/pair` arrives first is held until the second one,
        // which is answered first: a genuine reverse-order arrival. Each
        // caller must get its own tag back.
        thread::scope(|scope| {
            for tag in ["a", "b"] {
                let handle = &handle;
                scope.spawn(move || {
                    let result = handle.request("test/pair", json!({ "tag": tag })).unwrap();
                    assert_eq!(result, json!(tag));
                });
            }
        });

        // An explicit `"result": null` is a null answer, not an error.
        assert_eq!(
            handle.request("test/nullresult", json!({})).unwrap(),
            Value::Null
        );
        // An error response is an Err naming the message.
        let error = handle.request("test/error", json!({})).unwrap_err();
        assert!(error.to_string().contains("boom"), "{error}");

        let snapshot = handle.snapshot().unwrap();
        assert_eq!(snapshot.pending_operations, 0);
        assert!(snapshot.last_message.is_some());
        handle.shutdown().unwrap();
    }

    #[test]
    fn progress_tokens_gate_readiness_and_server_requests_are_answered() {
        needs_python!();
        let mock = Mock::new("progress");
        let handle = mock.spawn(test_settings(), Box::new(StandardIdle));
        initialize(&handle);
        assert!(handle.snapshot().unwrap().ready);
        handle.wait_ready(Duration::from_secs(1)).unwrap();

        // The mock emits the progress frame BEFORE its reply, so when the
        // request returns the owner has already processed it (same
        // producer, FIFO): no timing assumption.
        handle
            .request("test/progress", json!({ "action": "create", "token": "t1" }))
            .unwrap();
        let snapshot = handle.snapshot().unwrap();
        assert_eq!(snapshot.open_progress_tokens, 1);
        assert!(!snapshot.ready, "a created token is outstanding work");

        // The create was a server request; the owner must have answered it
        // with an explicit null result (the mock echoes our reply back). Our
        // reply was queued before this echo request, so the mock has read
        // and echoed it by the time it answers here.
        handle.request("test/echo", json!({})).unwrap();
        let events = handle.capture_events().unwrap();
        let echo = echo_for(&events, 7001).expect("create reply echoed");
        assert_eq!(echo.pointer("/params/result"), Some(&Value::Null));

        let waiter = thread::spawn({
            let handle = handle.clone_for_test();
            move || handle.wait_ready(Duration::from_secs(5))
        });
        handle
            .request("test/progress", json!({ "action": "begin", "token": "t2" }))
            .unwrap();
        assert_eq!(handle.snapshot().unwrap().open_progress_tokens, 2);
        handle
            .request("test/progress", json!({ "action": "end", "token": "t1" }))
            .unwrap();
        assert!(!handle.snapshot().unwrap().ready, "t2 still open");
        handle
            .request("test/progress", json!({ "action": "end", "token": "t2" }))
            .unwrap();
        waiter.join().unwrap().unwrap();
        assert!(handle.snapshot().unwrap().ready);
        handle.shutdown().unwrap();
    }

    #[test]
    fn policy_composes_with_standard_progress() {
        needs_python!();
        let mock = Mock::new("policy");
        let gate = Arc::new(AtomicBool::new(false));
        let handle = mock.spawn(test_settings(), Box::new(Gate(Arc::clone(&gate))));
        initialize(&handle);

        // No tokens, but the policy says not ready: not ready.
        assert!(!handle.snapshot().unwrap().ready);
        let waiter = thread::spawn({
            let handle = handle.clone_for_test();
            move || handle.wait_ready(Duration::from_secs(5))
        });
        handle
            .request("test/gate", json!({ "open": true }))
            .unwrap();
        waiter.join().unwrap().unwrap();
        assert!(handle.snapshot().unwrap().ready);

        // Policy ready, but a token open: the policy cannot override
        // standard progress.
        handle
            .request("test/progress", json!({ "action": "begin", "token": "x" }))
            .unwrap();
        assert!(!handle.snapshot().unwrap().ready);
        handle
            .request("test/progress", json!({ "action": "end", "token": "x" }))
            .unwrap();
        assert!(handle.snapshot().unwrap().ready);
        handle.shutdown().unwrap();
    }

    #[test]
    fn silence_trips_the_backstop_and_the_connection_survives() {
        needs_python!();
        let mock = Mock::new("silence");
        // Python's start-up under a parallel test run can exceed a few
        // hundred milliseconds, so the backstop is a full second.
        let settings = Settings {
            backstop: Duration::from_secs(1),
            ..test_settings()
        };
        let handle = mock.spawn(settings, Box::new(StandardIdle));
        initialize(&handle);

        let started = Instant::now();
        let error = handle.request("test/never", json!({})).unwrap_err();
        let elapsed = started.elapsed();
        let text = error.to_string();
        assert!(text.contains("python"), "names the server: {text}");
        assert!(text.contains("inactivity backstop"), "{text}");
        assert!(text.contains("last message: response"), "{text}");
        assert!(
            elapsed >= Duration::from_secs(1) && elapsed < Duration::from_secs(4),
            "tripped after {elapsed:?}"
        );

        // The server is silent, not dead: the connection stays usable.
        assert_eq!(
            handle.request("test/echo", json!({})).unwrap(),
            json!("ok")
        );
        handle.shutdown().unwrap();
    }

    #[test]
    fn activity_extends_the_backstop() {
        needs_python!();
        let mock = Mock::new("chatter");
        let settings = Settings {
            backstop: Duration::from_secs(1),
            ..test_settings()
        };
        let handle = mock.spawn(settings, Box::new(StandardIdle));
        initialize(&handle);

        // Six notifications 400 ms apart, then the reply: 2.4 s of a 1 s
        // backstop, never silent for more than 400 ms.
        let result = handle
            .request("test/chatter", json!({ "count": 6, "period_ms": 400 }))
            .unwrap();
        assert_eq!(result, json!("done"));
        handle.shutdown().unwrap();
    }

    #[test]
    fn a_long_idle_connection_accepts_a_new_request() {
        needs_python!();
        let mock = Mock::new("idle");
        let settings = Settings {
            backstop: Duration::from_secs(1),
            ..test_settings()
        };
        let handle = mock.spawn(settings, Box::new(StandardIdle));
        initialize(&handle);
        // Idle for longer than the backstop: the next operation starts its
        // own clock at admission.
        thread::sleep(Duration::from_millis(2500));
        assert_eq!(
            handle.request("test/echo", json!({})).unwrap(),
            json!("ok")
        );
        handle.shutdown().unwrap();
    }

    #[test]
    fn eof_fails_every_waiter_and_later_calls() {
        needs_python!();
        let mock = Mock::new("eof");
        let handle = mock.spawn(test_settings(), Box::new(StandardIdle));
        initialize(&handle);
        assert!(handle.notify("test/hello", json!({})).is_ok());

        let pending = thread::spawn({
            let handle = handle.clone_for_test();
            move || handle.request("test/never", json!({}))
        });
        // The mock exits without replying.
        let error = handle.request("test/die", json!({})).unwrap_err();
        assert!(error.to_string().contains("failed"), "{error}");
        let error = pending.join().unwrap().unwrap_err();
        assert!(error.to_string().contains("failed"), "{error}");

        let started = Instant::now();
        let error = handle.request("test/echo", json!({})).unwrap_err();
        assert!(error.to_string().contains("failed"), "{error}");
        assert!(handle.notify("test/hello", json!({})).is_err());
        assert!(handle.wait_ready(Duration::from_secs(5)).is_err());
        assert!(started.elapsed() < Duration::from_secs(1), "fails fast");
        let started = Instant::now();
        handle.shutdown().unwrap();
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn shutdown_is_prompt_with_the_writer_genuinely_blocked() {
        needs_python!();
        let mock = Mock::new("blocked");
        let handle = mock.spawn(test_settings(), Box::new(StandardIdle));
        initialize(&handle);
        // The mock answers, then never reads stdin again.
        handle.request("test/stop-reading", json!({})).unwrap();

        // Far more than a pipe buffer: the writer blocks inside the write.
        let big = "x".repeat(600 * 1024);
        let blocked = thread::spawn({
            let handle = handle.clone_for_test();
            move || handle.notify("test/big", json!({ "payload": big }))
        });
        // Let the writer reach the blocked write (a timing fixture for the
        // mock, not a settle wait: shutdown must be correct either way).
        thread::sleep(Duration::from_millis(200));

        let started = Instant::now();
        handle.shutdown().unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "shutdown took {:?}",
            started.elapsed()
        );
        let error = blocked.join().unwrap().unwrap_err();
        assert!(
            error.to_string().contains("not written")
                || error.to_string().contains("failed")
                || error.to_string().contains("closed")
                || error.to_string().contains("shutting down"),
            "{error}"
        );
    }

    #[test]
    fn required_reply_overflow_terminates_the_connection() {
        needs_python!();
        let mock = Mock::new("storm");
        let settings = Settings {
            writer_cap: 4,
            ..test_settings()
        };
        let handle = mock.spawn(settings, Box::new(StandardIdle));
        initialize(&handle);

        // The mock answers, then reads only the HEADER of our next frame,
        // then emits eight server requests and never reads again. Our next
        // frame is oversized, so the writer is stuck inside its body while
        // the eight required replies queue behind it: the fifth cannot be
        // queued, and that terminates the connection — which is what
        // finally fails this blocked notify.
        assert_eq!(
            handle.request("test/storm", json!({ "n": 8 })).unwrap(),
            json!("stormed")
        );
        let big = "x".repeat(600 * 1024);
        let error = handle
            .notify("test/big", json!({ "payload": big }))
            .unwrap_err();
        assert!(
            error.to_string().contains("not written") || error.to_string().contains("failed"),
            "{error}"
        );
        let error = handle.request("test/never", json!({})).unwrap_err();
        let text = error.to_string();
        assert!(text.contains("could not queue the reply"), "{text}");
        let snapshot = handle.snapshot().unwrap();
        assert!(snapshot.failure.is_some(), "{snapshot:?}");
        let started = Instant::now();
        handle.shutdown().unwrap();
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn shutdown_works_under_admission_pressure() {
        needs_python!();
        let mock = Mock::new("saturate");
        let settings = Settings {
            inbound_cap: 2,
            pending_cap: 2,
            ..test_settings()
        };
        let handle = mock.spawn(settings, Box::new(StandardIdle));
        initialize(&handle);

        let stop = AtomicBool::new(false);
        let first_refusal_for_shutdown = std::sync::Mutex::new(None::<Instant>);
        let flagged_at = thread::scope(|scope| {
            for _ in 0..4 {
                let handle = &handle;
                let stop = &stop;
                let first = &first_refusal_for_shutdown;
                scope.spawn(move || {
                    while !stop.load(Ordering::SeqCst) {
                        match handle.request("test/never", json!({})) {
                            Ok(_) => panic!("test/never must not be answered"),
                            Err(error) => {
                                let text = error.to_string();
                                if text.contains("shutting down") || text.contains("closed") {
                                    first.lock().unwrap().get_or_insert(Instant::now());
                                    return;
                                }
                                // Refused for capacity or failed at teardown
                                // are both honest; keep pressing.
                            }
                        }
                    }
                });
            }
            thread::sleep(Duration::from_millis(100));
            // The terminate path exactly as `shutdown`/`Drop` take it, minus
            // the join (the scope holds shared borrows).
            let flagged_at = Instant::now();
            handle.terminate.store(true, Ordering::SeqCst);
            let _ = handle.inbox.try_send(OwnerMsg::Admit(Command::Shutdown));
            thread::sleep(Duration::from_millis(1500));
            stop.store(true, Ordering::SeqCst);
            flagged_at
        });
        let first = first_refusal_for_shutdown
            .lock()
            .unwrap()
            .expect("callers saw the shutdown");
        assert!(
            first.duration_since(flagged_at) < Duration::from_secs(2),
            "owner noticed the flag after {:?}",
            first.duration_since(flagged_at)
        );
        let started = Instant::now();
        handle.shutdown().unwrap();
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn a_notification_flood_between_calls_is_consumed_and_bounded() {
        needs_python!();
        let mock = Mock::new("flood");
        let handle = mock.spawn(test_settings(), Box::new(StandardIdle));
        initialize(&handle);

        // Arm: the mock opens a progress token, replies, then — with no
        // request of ours outstanding — emits 10 000 notifications and ends
        // the token. Readiness returns only once the owner has processed the
        // `end`, which the reader delivered after all 10 000 (same producer,
        // FIFO). Nothing pumps; the caller only waits for readiness.
        handle
            .request("test/flood-arm", json!({ "n": 10_000, "token": "flood" }))
            .unwrap();
        assert!(!handle.snapshot().unwrap().ready);
        handle.wait_ready(Duration::from_secs(20)).unwrap();

        let snapshot = handle.snapshot().unwrap();
        assert!(snapshot.ready);
        assert_eq!(snapshot.pending_operations, 0);
        let events = handle.capture_events().unwrap();
        assert!(events.len() <= 200, "event log capped: {}", events.len());
        assert_eq!(
            events.last().and_then(|e| e.pointer("/params/value/kind")),
            Some(&json!("end")),
            "the last event is the token's end"
        );
        assert!(
            events.iter().any(|e| e["method"] == "test/noise"),
            "the flood was recorded"
        );
        // The owner is healthy afterwards.
        assert_eq!(
            handle.request("test/echo", json!({})).unwrap(),
            json!("ok")
        );
        handle.shutdown().unwrap();
    }

    impl ServerHandle {
        /// A second caller-side handle onto the same connection, for tests
        /// that need a call outstanding on another thread while this one
        /// keeps driving. It never owns the owner thread, so its `Drop` is
        /// a no-op; teardown stays with the original.
        fn clone_for_test(&self) -> ServerHandle {
            ServerHandle {
                label: self.label.clone(),
                inbox: self.inbox.clone(),
                inbound_cap: self.inbound_cap,
                terminate: Arc::clone(&self.terminate),
                owner: None,
                join_bound: self.join_bound,
            }
        }
    }

    /// The scripted server behind every test above.
    const MOCK_PY: &str = r#"
import json, sys, time

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

def notify(method, params):
    send({"jsonrpc": "2.0", "method": method, "params": params})

def progress(token, kind):
    notify("$/progress", {"token": token, "value": {"kind": kind}})

held_pair = None
next_server_id = 7001

while True:
    msg = read_msg()
    if msg is None:
        break
    if "method" in msg and "id" in msg:
        method = msg["method"]
        params = msg.get("params") or {}
        if method == "initialize":
            reply(msg, {"capabilities": {}})
        elif method == "test/pair":
            if held_pair is None:
                held_pair = msg
            else:
                reply(msg, params["tag"])
                reply(held_pair, held_pair["params"]["tag"])
                held_pair = None
        elif method == "test/nullresult":
            reply(msg, None)
        elif method == "test/error":
            send({"jsonrpc": "2.0", "id": msg["id"],
                  "error": {"code": -32000, "message": "boom"}})
        elif method == "test/never":
            pass
        elif method == "test/die":
            sys.exit(0)
        elif method == "test/chatter":
            for i in range(int(params["count"])):
                time.sleep(params["period_ms"] / 1000.0)
                notify("test/tick", {"i": i})
            reply(msg, "done")
        elif method == "test/progress":
            token = params["token"]
            action = params["action"]
            if action == "create":
                send({"jsonrpc": "2.0", "id": next_server_id,
                      "method": "window/workDoneProgress/create",
                      "params": {"token": token}})
                next_server_id += 1
            else:
                progress(token, action)
            reply(msg, "sent")
        elif method == "test/gate":
            notify("test/gate", {"open": bool(params["open"])})
            reply(msg, "sent")
        elif method == "test/flood-arm":
            token = params["token"]
            progress(token, "begin")
            reply(msg, "armed")
            for i in range(int(params["n"])):
                notify("test/noise", {"i": i})
            progress(token, "end")
        elif method == "test/stop-reading":
            reply(msg, "stopped")
            while True:
                time.sleep(3600)
        elif method == "test/storm":
            reply(msg, "stormed")
            # Observe the client's next frame arriving (its header only),
            # then demand replies without ever reading again.
            read_headers()
            for i in range(int(params["n"])):
                send({"jsonrpc": "2.0", "id": 8000 + i, "method": "test/ping-me",
                      "params": {}})
            while True:
                time.sleep(3600)
        elif method == "shutdown":
            reply(msg, None)
        else:
            reply(msg, "ok")
    elif "method" in msg:
        if msg["method"] == "exit":
            sys.exit(0)
    elif "id" in msg:
        # A response from the client: echo it back so tests can assert it.
        notify("test/response-echo", msg)
"#;
}
