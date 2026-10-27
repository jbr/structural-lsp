//! The messages between the handle, the owner and the writer, and the
//! owner's records of what is outstanding.

use super::*;

pub(super) enum OwnerMsg {
    FromServer(Incoming),
    /// Commands are waiting in the command queue: a wake-up and nothing
    /// else. Best effort, like [`OwnerMsg::Written`]: a doorbell that finds
    /// the inbox full is dropped, because a full inbox wakes the owner
    /// anyway and the owner drains the command queue on every wake.
    Doorbell,
    /// The writer wrote (or acknowledged) this tracked frame; the owner may
    /// forget it. Best effort: a full inbox loses it, and the entry then
    /// lingers until its backstop, harmlessly.
    Written(u64),
    /// The writer could not write a frame; the connection is dead.
    WriteFailed(String),
}

pub(super) enum Command {
    Request {
        method: String,
        params: Value,
        reply: Sender<Result<Value>>,
        admitted_at: Instant,
        /// A semantic request ([`ServerHandle::query`]): written only when
        /// the server is ready, and asked again when it is answered while
        /// work begun since the write is outstanding.
        guarded: bool,
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

pub(super) struct WriterJob {
    pub(super) frame: Vec<u8>,
    /// Present for notifications: the caller is told the real outcome of
    /// the write. Requests carry none — their outcome is the response.
    pub(super) ack: Option<Sender<Result<()>>>,
    /// The owner's id for a tracked write, reported back once done.
    pub(super) tracked: Option<u64>,
}

/// A notification handed to the writer whose caller is still waiting for
/// the ack: the inactivity backstop applies to it as to a request, so a
/// write blocked on a wedged server fails its caller instead of hanging.
pub(super) struct PendingWrite {
    pub(super) method: String,
    pub(super) ack: Sender<Result<()>>,
    pub(super) admitted_at: Instant,
    pub(super) backstop: Duration,
}

pub(super) struct PendingRequest {
    pub(super) method: String,
    /// `None` for the owner's own `shutdown` request.
    pub(super) reply: Option<Sender<Result<Value>>>,
    pub(super) admitted_at: Instant,
    pub(super) backstop: Duration,
    /// Present for a semantic request: what it needs to be written again,
    /// and the readiness generation it was written under.
    pub(super) guard: Option<Guard>,
}

/// A semantic request's params, kept so it can be written again, and the
/// readiness generation current when it was written (docs/correctness.md).
pub(super) struct Guard {
    pub(super) params: Value,
    pub(super) generation: u64,
}

/// A semantic request waiting for the server to be ready before it is
/// written: admitted while work was outstanding, or set aside because work
/// began between its write and its response.
pub(super) struct Deferred {
    pub(super) method: String,
    pub(super) params: Value,
    pub(super) reply: Sender<Result<Value>>,
    pub(super) admitted_at: Instant,
    pub(super) backstop: Duration,
}

pub(super) struct Waiter {
    pub(super) reply: Sender<Result<()>>,
    pub(super) admitted_at: Instant,
    pub(super) backstop: Duration,
}
