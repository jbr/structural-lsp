//! The lifecycle of one planned language server.
//!
//! A slot exists for every **plan** the workspace selected, whether or not a
//! server is running: the routing question — which files would this server
//! serve — is answered by the adapter, and only the state changes over the
//! slot's life.
//!
//! A slot holds the plan's whole **ladder**, not one adapter: the preferred
//! candidate first, then the alternatives in rank order
//! (`docs/discovery.md`). A launch walks it, and the walk is bounded by the
//! list — there is no retry loop and no timer. Each candidate that fails to
//! initialize is retained as an [`Attempt`], so a fallback can never hide
//! the reason it fell back, and a slot that runs its second candidate can
//! still say what happened to its first.
//!
//! The state sits behind its **own** lock rather than the workspace's, so a
//! demand can read it and wait on a launch without holding the workspace
//! lock. That is what lets a server start while the workspace is otherwise
//! free: a launcher holds nothing while it spawns and initializes, and takes
//! the workspace lock only to publish the finished server.

use crate::lsp::adapter::Adapter;
use anyhow::{Result, bail};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

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
    /// Every candidate on the ladder failed, with this reason — which names
    /// each one that was tried and what it said. A demand reports that
    /// rather than starting another attempt; the idle sweep clears it on
    /// the same clock that expires a server, so a later demand walks the
    /// ladder again (`docs/observation.md`).
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
    /// The ladder was walked and nothing on it started: report this rather
    /// than retry. Bounded by the list, which is what "bounded selection,
    /// not an indefinite restart loop" means here.
    Failed(String),
    /// Nobody was starting it, so this demand became the launcher. The guard
    /// leaves `Starting` on every exit path, including a panic, and owns its
    /// slot so the launch can run on a thread of its own.
    Lead(Launch),
    /// The host is stopping. No launch may begin, and none did: the slot is
    /// left exactly as it was, so a demand after a shutdown costs a load
    /// rather than a thread.
    Refused,
}

/// A candidate that was tried and did not initialize.
///
/// The install hint is captured here rather than looked up later, because
/// the adapter that failed may no longer be the slot's active one by the
/// time anybody renders this.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attempt {
    pub server: String,
    pub reason: String,
    pub hint: String,
}

impl Attempt {
    /// One line, for the message a caller gets.
    pub fn explain(&self) -> String {
        format!("{}: {}; to fix: {}", self.server, self.reason, self.hint)
    }
}

/// One plan's lifecycle: its ladder of candidates, and where it is.
pub struct ServerSlot {
    /// The plan's candidates in rank order, fixed when the slot is laid
    /// out. Never empty.
    candidates: Vec<Arc<dyn Adapter>>,
    /// Which candidate the slot currently stands for: the one running, the
    /// one being tried, or — before any launch and after a stop —
    /// the preferred one. Read without a lock, because the candidate list
    /// is immutable and this is only an index into it.
    active: AtomicUsize,
    /// The last strong signal addressed to this slot: the moment it was
    /// laid out, refreshed by a bind (which addresses every slot) and by
    /// every query that routes to it, and never by a filesystem batch —
    /// activity is not a keepalive (`docs/observation.md`). The sweep
    /// expires a slot when this plus the idle interval has passed.
    last_use: Mutex<Instant>,
    /// Bumped by every stop (`mark_cold`). The identity a launch is elected
    /// under: a launch that finishes after a stop sees a different
    /// generation and must not publish over the slot — the publish-path
    /// guarantee residency owes (`docs/observation.md`). Bumps happen under
    /// the workspace lock, and the comparison at publish reads it there
    /// too, so the two cannot interleave.
    generation: AtomicU64,
    /// The candidates already tried and rejected in this launch, retained
    /// whether the walk went on to succeed or ran out.
    attempts: Mutex<Vec<Attempt>>,
    /// The host's one claim, shared rather than copied — the same atomic every
    /// other slot sees. Election consults it, so a demand arriving after a
    /// shutdown neither elects a launcher nor spawns a thread for a launch
    /// that was born refusing.
    stopped: Arc<AtomicBool>,
    state: Mutex<SlotState>,
    /// Signalled on every transition out of [`SlotState::Starting`], so a
    /// waiter is never left blocked on a launcher that will not wake it.
    launched: Condvar,
}

impl ServerSlot {
    /// A slot for one plan's ladder, preferred candidate first, standing at
    /// `now`: the moment the tree called for this plan, which is where its
    /// idle clock starts.
    ///
    /// Panics on an empty ladder: a plan is built from a non-empty
    /// candidate list, and a slot standing for no server could answer no
    /// routing question at all.
    pub fn new(candidates: Vec<Arc<dyn Adapter>>, stopped: Arc<AtomicBool>, now: Instant) -> Self {
        assert!(
            !candidates.is_empty(),
            "a slot is laid out for a plan, and a plan has at least one candidate"
        );
        Self {
            candidates,
            active: AtomicUsize::new(0),
            last_use: Mutex::new(now),
            generation: AtomicU64::new(0),
            attempts: Mutex::new(Vec::new()),
            stopped,
            state: Mutex::new(SlotState::Cold),
            launched: Condvar::new(),
        }
    }

    /// The candidate this slot currently stands for. The list is immutable,
    /// so a routing question needs no lock — which is what lets a *cold*
    /// slot still say which languages it would serve.
    pub fn adapter(&self) -> &dyn Adapter {
        self.candidates[self.active.load(Ordering::SeqCst)].as_ref()
    }

    /// A shared handle to the active candidate, for a launch that must own
    /// one.
    pub fn adapter_arc(&self) -> Arc<dyn Adapter> {
        Arc::clone(&self.candidates[self.active.load(Ordering::SeqCst)])
    }

    /// How many candidates the ladder holds. The bound on a launch's walk.
    pub fn rungs(&self) -> usize {
        self.candidates.len()
    }

    /// Stand for candidate `rung` and hand the launcher its adapter, so
    /// every message — the slot's name, its install hint, the languages it
    /// routes by — names the candidate actually being tried.
    pub fn attempting(&self, rung: usize) -> Arc<dyn Adapter> {
        self.active.store(rung, Ordering::SeqCst);
        Arc::clone(&self.candidates[rung])
    }

    /// Retain a candidate that did not initialize, and move on. Retained on
    /// success as well as on exhaustion: a fallback that hides the reason
    /// it fell back is worse than no fallback
    /// (`docs/discovery.md`).
    pub fn attempted(&self, adapter: &dyn Adapter, reason: impl Into<String>) {
        self.attempts().push(Attempt {
            server: adapter.name().to_string(),
            reason: reason.into(),
            hint: adapter.install_hint().to_string(),
        });
    }

    /// The candidates tried and rejected before the slot reached its
    /// current state.
    pub fn failed_attempts(&self) -> Vec<Attempt> {
        self.attempts().clone()
    }

    fn attempts(&self) -> MutexGuard<'_, Vec<Attempt>> {
        self.attempts.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn name(&self) -> &str {
        self.adapter().name()
    }

    /// The name of every candidate, in rank order — what the ladder would
    /// be walked as, independent of where it got to.
    pub fn candidate_names(&self) -> Vec<&str> {
        self.candidates
            .iter()
            .map(|adapter| adapter.name())
            .collect()
    }

    pub fn state(&self) -> SlotState {
        self.lock().clone()
    }

    /// Refresh this slot's idle clock. Strong signals only: a bind touches
    /// every slot, a query the ones its scope routes to, and a filesystem
    /// batch nothing (`docs/observation.md`).
    pub fn touch(&self, now: Instant) {
        *self.lock_last_use() = now;
    }

    /// When this slot was last addressed by a strong signal.
    pub fn last_use(&self) -> Instant {
        *self.lock_last_use()
    }

    /// The generation this slot currently stands at: what a launch is
    /// elected under, and what a stop bumps.
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }

    fn lock_last_use(&self) -> MutexGuard<'_, Instant> {
        self.last_use.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Decide what this demand should do about the state, holding the lock
    /// across the decision so that becoming the launcher is atomic with
    /// leaving `Cold`.
    pub fn demand(self: &Arc<Self>) -> Demand {
        if self.stopped.load(Ordering::SeqCst) {
            return Demand::Refused;
        }
        let mut state = self.lock();
        match &*state {
            SlotState::Running => Demand::Ready,
            SlotState::Starting => Demand::Join,
            SlotState::Failed(reason) => Demand::Failed(reason.clone()),
            // Election records the generation this launch is elected under:
            // a stop that follows bumps the slot's, and the launch's
            // publish compares then (`docs/observation.md`).
            SlotState::Cold => {
                *state = SlotState::Starting;
                Demand::Lead(Launch {
                    slot: Arc::clone(self),
                    generation: self.generation(),
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
            // The only way out of `Starting` besides the launch's own
            // endings is a stop (`mark_cold`): the launch this demand
            // elected or joined was taken from under it, and the next
            // query starts the server again.
            SlotState::Cold => bail!(
                "the server was stopped while it was starting; the next query starts it again"
            ),
            // Unreachable: the loop above exits only when the state has
            // left `Starting`. Here for exhaustiveness of the enum.
            SlotState::Starting => bail!("the launch is still under way"),
        }
    }

    fn lock(&self) -> MutexGuard<'_, SlotState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Nothing is running and a later demand may start a server again. Used
    /// by eviction and by shutdown alike, so the two cannot disagree about
    /// what stopped means.
    ///
    /// The ladder resets with it: a later demand walks it from the top, and
    /// the attempts it clears described a launch that is over. A candidate
    /// that failed once is not thereby excluded forever — clearing a
    /// failure on the clock is the residency policy
    /// (`docs/observation.md`).
    ///
    /// The generation bump is what an in-flight launch notices: its
    /// election carried the previous generation, so a later publish
    /// compares and refuses — a launch never publishes over a slot a stop
    /// has taken from under it (`docs/observation.md`).
    pub fn mark_cold(&self) {
        self.active.store(0, Ordering::SeqCst);
        self.generation.fetch_add(1, Ordering::SeqCst);
        self.attempts().clear();
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
    /// The slot generation this launch was elected under. A stop bumps the
    /// slot's generation; a launch finishing after such a stop sees the
    /// mismatch and must not touch the slot — not publish over it, and not
    /// mark it failed either, because the state it would clobber belongs to
    /// whatever stopped it or to a launch elected after
    /// (`docs/observation.md`).
    generation: u64,
    finished: bool,
}

impl Launch {
    /// The slot whose ladder this launch walks.
    pub fn slot(&self) -> &Arc<ServerSlot> {
        &self.slot
    }

    /// A shared handle to the candidate the launch is currently trying.
    pub fn adapter_arc(&self) -> Arc<dyn Adapter> {
        self.slot.adapter_arc()
    }

    pub fn name(&self) -> &str {
        self.slot.name()
    }

    /// The ladder ran out: every candidate was tried, and each one's reason
    /// is in the failure rather than only the last.
    pub fn exhausted(self) {
        let attempts = self.slot.failed_attempts();
        let reason = attempts
            .iter()
            .map(Attempt::explain)
            .collect::<Vec<_>>()
            .join("\n    ");
        self.failed(match attempts.len() {
            0 | 1 => reason,
            n => format!("{n} candidates were tried and none started:\n    {reason}"),
        });
    }

    /// The server is in the workspace: leave `Starting` for `Running`.
    pub fn published(mut self) {
        self.finished = true;
        self.slot.mark(SlotState::Running);
    }

    /// The launch failed: record why, and wake the waiters. A launch
    /// superseded by a stop stays silent — the slot it would mark is no
    /// longer its slot.
    pub fn failed(mut self, reason: impl Into<String>) {
        self.finished = true;
        if self.superseded() {
            return;
        }
        self.slot.mark(SlotState::Failed(reason.into()));
    }

    /// A stop took the slot from under this launch: the slot's generation
    /// moved past the one this launch was elected under. Publication
    /// checks this under the workspace lock before it writes anything.
    pub(crate) fn superseded(&self) -> bool {
        self.generation != self.slot.generation()
    }
}

impl Drop for Launch {
    fn drop(&mut self) {
        if self.finished || self.superseded() {
            return;
        }
        self.slot.mark(SlotState::Failed(
            "the launcher stopped without publishing a server".to_string(),
        ));
    }
}
