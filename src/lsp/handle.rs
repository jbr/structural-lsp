//! A language server on its own threads, addressed through a blocking handle.
//!
//! Nothing here waits for a tool call to make progress. Three threads per
//! server:
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
//! A busy server can also be a *quiet* one: rust-analyzer says nothing for
//! minutes while cargo builds proc-macros, with a progress token open. So
//! while work is pending and the server has been silent for
//! [`Settings::probe_after`], the owner sends a **liveness probe** — a
//! request with a `$/`-prefixed method the server does not implement, which
//! the LSP spec obliges it to answer with `MethodNotFound`. The answer is
//! activity like any other and extends every deadline. The backstop thus
//! means "silent *and* not answering probes for this long". A server that
//! answers probes but never finishes is alive and stuck, which no timer can
//! tell from slow; that boundary is deliberate.
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
//!
//! [`Stopper::force`] skips the remaining graceful window and abandons
//! pending work. On Unix, every server starts in its own process group;
//! teardown kills that group before reaping the direct child. Descendants
//! that deliberately detach are outside this guarantee. Other platforms
//! retain direct-child cleanup only. Stop capabilities carry flags, never
//! PIDs, and remain safe after teardown. Our own uncatchable death still
//! requires an external supervisor for cleanup.

use crate::lsp::adapter::{ReadinessPolicy, StandardIdle};
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

mod child;
mod owner;
mod progress;
mod startup;
mod workers;
use child::OwnedChild;
use owner::Owner;
use progress::ProgressTokens;
use workers::{reader_loop, writer_loop};

#[cfg(test)]
mod tests;

/// The longest the owner waits for a message before re-checking the
/// terminate flag and overdue operations. This is a control-plane wake-up,
/// not a settle delay: nothing is processed on a tick that carried no
/// message, and readiness waits complete on the message that satisfies them,
/// not on a tick.
pub const CONTROL_TICK: Duration = Duration::from_millis(250);

/// The liveness probe: a method no server implements, chosen under the `$/`
/// prefix so the spec's `MethodNotFound` obligation applies.
pub const PROBE_METHOD: &str = "$/structural/ping";

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
    /// Whether `window/workDoneProgress/create` alone counts as outstanding
    /// work, as it does for every server that begins the work it reserved a
    /// token for. `false` for a server that reserves a token and never
    /// reports through it, which would otherwise never be idle
    /// (docs/servers.md).
    pub progress_create_is_work: bool,
    /// Diagnostic event log entries retained; oldest are dropped first.
    pub event_cap: usize,
    /// The inactivity backstop for requests and notifications.
    pub backstop: Duration,
    /// Silence, with work pending, before a liveness probe is sent. One
    /// probe is outstanding at a time; the next follows another silence.
    pub probe_after: Duration,
    /// How long a graceful `shutdown`/`exit` may take before the child is
    /// killed regardless.
    pub shutdown_grace: Duration,
    /// What to answer a server's `workspace/configuration` request with,
    /// one copy per requested item. `Null` is "no opinion", which hands the
    /// server its own defaults.
    ///
    /// The one field here that is not a bound. It belongs to the
    /// connection because the answer must be ready when the request
    /// arrives, which is before any caller is involved: deno 2.9.6 asks
    /// during startup and serves **nothing at all** for the rest of the
    /// session when the answer is `null` (docs/servers.md).
    pub configuration: Value,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            inbound_cap: 1024,
            writer_cap: 64,
            pending_cap: 256,
            progress_token_cap: 4096,
            progress_create_is_work: true,
            event_cap: 2000,
            backstop: Duration::from_secs(90),
            probe_after: Duration::from_secs(20),
            shutdown_grace: Duration::from_secs(5),
            configuration: Value::Null,
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
    force: Arc<AtomicBool>,
    owner: Option<JoinHandle<()>>,
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
        startup::spawn(command, args, settings, policy, |name, task| {
            thread::Builder::new().name(name.into()).spawn(task)
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
            Err(_) => bail!(
                "connection to {} closed while waiting for readiness",
                self.label
            ),
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

    /// Whether this client should send `workspace/didChangeWatchedFiles`
    /// to the server: the retained registration state, held on the
    /// owner thread that sees the server's registration requests.
    /// A closed connection has no state to consult; the next
    /// operation on it reports the failure.
    pub fn watched_files(&self) -> bool {
        let (reply, outcome) = mpsc::channel();
        let _ = self.admit(Command::WatchedFiles { reply });
        outcome.recv().unwrap_or(false)
    }

    /// Shut the server down: a graceful `shutdown`/`exit` bounded by
    /// `shutdown_grace`, then kill and reap. Waits for the owner to finish;
    /// unlike worker joins, this wait never detaches the process owner.
    /// A retained [`Stopper`] can independently escalate while this waits.
    pub fn shutdown(mut self) -> Result<()> {
        self.begin_teardown();
        Ok(())
    }

    /// Ask the server to stop, without waiting for it.
    ///
    /// The terminate flag is the path that cannot fail — the owner checks it
    /// on every wake; the queued command is the fast one when the inbox has
    /// room. This is what a host uses on a child whose owning thread is still
    /// inside `initialize`: that thread gets its error, and the process is
    /// already going away.
    pub fn terminate(&self) {
        self.terminate.store(true, Ordering::SeqCst);
        let _ = self.inbox.try_send(OwnerMsg::Admit(Command::Shutdown));
    }

    /// Enough to stop this server without owning its connection — cloned out
    /// of the handle so a host can hold it while the server's own thread is
    /// still starting it.
    pub fn stopper(&self) -> Stopper {
        Stopper {
            terminate: Arc::clone(&self.terminate),
            force: Arc::clone(&self.force),
            inbox: self.inbox.clone(),
        }
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
        self.terminate();
        // Completion must not be reported while the owner still owns a child.
        // Protocol grace and pipe-worker joins remain bounded independently.
        let _ = owner.join();
    }
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        self.begin_teardown();
    }
}

/// A nonblocking stop capability for the full lifetime of one server.
///
/// Safe to retain after teardown: it carries no PID and never signals from
/// the calling thread. The owner checks flags on every wake, at most one
/// [`CONTROL_TICK`] apart when idle, independently of caller/workspace locks.
#[derive(Clone)]
pub struct Stopper {
    terminate: Arc<AtomicBool>,
    force: Arc<AtomicBool>,
    inbox: SyncSender<OwnerMsg>,
}

impl Stopper {
    /// Abandon pending work and skip any remaining graceful shutdown window.
    /// Monotonic and idempotent, including concurrently with [`Self::stop`].
    /// On Unix the owner kills the owned process group, then reaps its direct
    /// child. Descendants that deliberately detach escape that group; on
    /// non-Unix platforms only the direct child is killed. This requests
    /// cleanup, not completion, and cannot protect against our own SIGKILL.
    pub fn force(&self) {
        self.force.store(true, Ordering::SeqCst);
        self.stop();
    }

    /// Idempotent, and safe on a server that is still initializing: the
    /// owning thread's request fails, and the process is torn down.
    pub fn stop(&self) {
        self.terminate.store(true, Ordering::SeqCst);
        let _ = self.inbox.try_send(OwnerMsg::Admit(Command::Shutdown));
    }
}

// --- messages between the threads ---

enum OwnerMsg {
    FromServer(Incoming),
    Admit(Command),
    /// The writer wrote (or acknowledged) this tracked frame; the owner may
    /// forget it. Best effort: a full inbox loses it, and the entry then
    /// lingers until its backstop, harmlessly.
    Written(u64),
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
    /// Whether the server wants `workspace/didChangeWatchedFiles`
    /// notifications, per the registration state it has asked
    /// this client to hold.
    WatchedFiles {
        reply: Sender<bool>,
    },
    Shutdown,
}

struct WriterJob {
    frame: Vec<u8>,
    /// Present for notifications: the caller is told the real outcome of
    /// the write. Requests carry none — their outcome is the response.
    ack: Option<Sender<Result<()>>>,
    /// The owner's id for a tracked write, reported back once done.
    tracked: Option<u64>,
}

/// A notification handed to the writer whose caller is still waiting for
/// the ack: the inactivity backstop applies to it as to a request, so a
/// write blocked on a wedged server fails its caller instead of hanging.
struct PendingWrite {
    method: String,
    ack: Sender<Result<()>>,
    admitted_at: Instant,
    backstop: Duration,
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

/// Wire tracing to stderr, enabled by `STRUCTURAL_WIRE` (or the spike's
/// `STRUCTURAL_SPIKE_WIRE`). Diagnostic only.
pub(crate) fn wire_debug_enabled() -> bool {
    ["STRUCTURAL_WIRE", "STRUCTURAL_SPIKE_WIRE"]
        .iter()
        .any(|name| std::env::var(name).is_ok_and(|value| value != "0"))
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
