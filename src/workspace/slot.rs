//! The lifecycle of one configured language server.
//!
//! A slot exists for every adapter the workspace selected, whether or not its
//! server is running: the routing question — which files would this server
//! serve — is answered by the adapter, and only the state changes over the
//! slot's life.
//!
//! The state sits behind its **own** lock rather than the workspace's, so a
//! demand can read it and wait on a launch without holding the workspace
//! lock. That is what lets a server start while the workspace is otherwise
//! free: a launcher holds nothing while it spawns and initializes, and takes
//! the workspace lock only to publish the finished server.

use crate::lsp::adapter::Adapter;
use anyhow::{Result, bail};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};

/// Where a slot is in its life. Only [`SlotState::Running`] answers queries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SlotState {
    /// Configured, not started. A demand starts it.
    Cold,
    /// A launch is under way. A second demand waits for it rather than
    /// starting another.
    Starting,
    /// The server is running, and the workspace holds it.
    Running,
    /// The last launch failed, with this reason. A later demand may retry.
    Failed(String),
}

impl SlotState {
    pub fn is_running(&self) -> bool {
        matches!(self, Self::Running)
    }

    /// The word a caller shows for this state.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Cold => "not started",
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Failed(_) => "failed",
        }
    }

    pub fn reason(&self) -> Option<&str> {
        match self {
            Self::Failed(reason) => Some(reason),
            _ => None,
        }
    }
}

/// What a demand finds, decided while the slot's lock is held.
pub enum Demand {
    /// Running: use it.
    Ready,
    /// A launch is under way: wait for it rather than starting a second.
    Join,
    /// The last launch failed: report this rather than retry.
    Failed(String),
    /// Nobody was starting it, so this demand became the launcher. The guard
    /// leaves `Starting` on every exit path, including a panic, and owns its
    /// slot so the launch can run on a thread of its own.
    Lead(Launch),
}

/// One configured server's lifecycle.
pub struct ServerSlot {
    adapter: Arc<dyn Adapter>,
    state: Mutex<SlotState>,
    /// Signalled on every transition out of [`SlotState::Starting`], so a
    /// waiter is never left blocked on a launcher that will not wake it.
    launched: Condvar,
    /// Set by a host shutdown before it sweeps. A launcher that finds it set
    /// stops the server it started instead of publishing it, so nothing is
    /// published into a host that is going away.
    abandoned: AtomicBool,
}

impl ServerSlot {
    pub fn new(adapter: Arc<dyn Adapter>) -> Self {
        Self {
            adapter,
            state: Mutex::new(SlotState::Cold),
            launched: Condvar::new(),
            abandoned: AtomicBool::new(false),
        }
    }

    /// Claim this slot for a shutdown: from here on, no server may be
    /// published into it, and no new one may be started.
    pub fn abandon(&self) {
        self.abandoned.store(true, Ordering::SeqCst);
    }

    pub fn abandoned(&self) -> bool {
        self.abandoned.load(Ordering::SeqCst)
    }

    /// The adapter is immutable, so a routing question needs no lock — which
    /// is what lets a *cold* slot still say which languages it would serve.
    pub fn adapter(&self) -> &dyn Adapter {
        self.adapter.as_ref()
    }

    /// A shared handle to the adapter, for a launch that must own one.
    pub fn adapter_arc(&self) -> Arc<dyn Adapter> {
        Arc::clone(&self.adapter)
    }

    pub fn name(&self) -> &str {
        self.adapter.name()
    }

    pub fn state(&self) -> SlotState {
        self.lock().clone()
    }

    /// Decide what this demand should do about the state, holding the lock
    /// across the decision so that becoming the launcher is atomic with
    /// leaving `Cold`.
    pub fn demand(self: &Arc<Self>) -> Demand {
        let mut state = self.lock();
        match &*state {
            SlotState::Running => Demand::Ready,
            SlotState::Starting => Demand::Join,
            SlotState::Failed(reason) => Demand::Failed(reason.clone()),
            SlotState::Cold => {
                *state = SlotState::Starting;
                Demand::Lead(Launch {
                    slot: Arc::clone(self),
                    finished: false,
                })
            }
        }
    }

    /// Wait for a launch already under way to finish. Returns what it ended
    /// as: the caller asks again rather than assuming it is running, because
    /// the slot may have failed since.
    pub fn wait_for_launch(&self) -> Result<()> {
        let mut state = self.lock();
        while matches!(*state, SlotState::Starting) {
            state = self
                .launched
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
        }
        match &*state {
            SlotState::Running => Ok(()),
            SlotState::Failed(reason) => bail!("{reason}"),
            other => bail!("the launch ended as {}", other.label()),
        }
    }

    fn lock(&self) -> MutexGuard<'_, SlotState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Nothing is running and a later demand may start a server again. Used
    /// by eviction and by shutdown alike, so the two cannot disagree about
    /// what stopped means.
    pub fn mark_cold(&self) {
        self.mark(SlotState::Cold);
    }

    fn mark(&self, state: SlotState) {
        {
            let mut current = self.lock();
            *current = state;
        }
        // Outside the lock: a waiter wakes into a slot it can read itself.
        self.launched.notify_all();
    }
}

/// A launcher's claim on a slot, released on every exit path.
///
/// The transition *out* of `Starting` happens later than entering it — after
/// a spawn and an `initialize` handshake — so a panic in between would leave
/// the slot `Starting` for good, with every waiter blocked on a wake that
/// never comes. This guard is what makes every launcher exit a wake: success,
/// failure, and unwind alike.
pub struct Launch {
    slot: Arc<ServerSlot>,
    finished: bool,
}

impl Launch {
    /// A shared handle to the adapter the launch will start.
    pub fn adapter_arc(&self) -> Arc<dyn Adapter> {
        self.slot.adapter_arc()
    }

    pub fn name(&self) -> &str {
        self.slot.name()
    }

    /// The server is in the workspace: leave `Starting` for `Running`.
    pub fn published(mut self) {
        self.finished = true;
        self.slot.mark(SlotState::Running);
    }

    /// The launch failed: record why, and wake the waiters.
    pub fn failed(mut self, reason: impl Into<String>) {
        self.finished = true;
        self.slot.mark(SlotState::Failed(reason.into()));
    }
}

impl Drop for Launch {
    fn drop(&mut self) {
        if !self.finished {
            self.slot.mark(SlotState::Failed(
                "the launcher stopped without publishing a server".to_string(),
            ));
        }
    }
}
