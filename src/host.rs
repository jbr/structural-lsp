//! The shared host: one process's workspaces, and the agents interested in
//! each of them.
//!
//! One host serves both front ends. The MCP process owns a host, a default
//! workspace, and its child servers; the embedded consumer owns one host
//! shared by its agents, each with its own effective cwd. Independently
//! spawned MCP processes do not share anything.
//!
//! A workspace is shared by every caller naming the same canonical root, so
//! those callers share one set of language-server processes. Unrelated
//! projects get their own, and operations on them do not serialize against
//! each other: the map lock below is taken only to find or create a
//! workspace, never to run a query.

use crate::clock::{Clock, SystemClock};
use crate::lsp::server::LspServer;
use crate::session::Session;
use crate::workspace::{ServerSlot, SlotState, Workspace, canonical_root};
use anyhow::Result;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, TryLockError, Weak};
use std::time::{Duration, Instant};

/// The root a workspace is shared under, for a caller working in `cwd`.
///
/// The project root above the working directory, bounded by the VCS root
/// and taking the outermost marker in that span
/// (`crate::discovery::roots`). Two agents in sibling members of one cargo
/// workspace therefore key the same and share servers, where keying on the
/// working directory verbatim gave each its own — and each of those
/// indexed the whole workspace anyway
/// (`reference/server-root-discovery.md`).
///
/// A wrong key costs **sharing, not correctness**: what a caller is told
/// is filtered against the session's view, which is the working directory
/// and does not consult the key (`docs/discovery.md`).
pub fn workspace_key(cwd: &Path) -> PathBuf {
    canonical_root(&crate::discovery::roots::Ancestry::gather(cwd).root().path)
}

/// Which agent — or front end — a workspace binding belongs to. Agent start
/// and end arrive as these identifiers, so releasing one agent's interest
/// never releases another's.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AgentId(String);

impl AgentId {
    pub fn new(id: impl Into<String>) -> Self {
        Self(id.into())
    }

    /// The MCP front end's own caller identity. One MCP process is one
    /// caller with one default workspace, exactly as an embedded agent is,
    /// so it registers through the same door.
    pub fn mcp() -> Self {
        Self("mcp".to_string())
    }
}

impl std::fmt::Display for AgentId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// One workspace and the agents interested in it.
///
/// The workspace sits behind its own lock, so a query holds only this
/// workspace — never the host — and two workspaces proceed independently. An
/// interested set that empties releases the workspace; a live [`Session`]
/// holds an `Arc` to this entry, so an operation already under way is never
/// cut short by a release.
///
/// The root and the slots are held out here, unlocked, so a launch can read
/// them and move a server's lifecycle without the workspace lock — which is
/// exactly what publication needs that lock for.
/// The host's shared lifetime: the shutdown manager every launch takes a
/// guard from, and the flag that says no launch may start.
///
/// Cloned into every entry, so a workspace acquired after a shutdown has
/// begun still reaches the same manager and gets the same answer. The flag is
/// the *claim* the sweep rests on: it is set before anything is stopped, and
/// a launcher checks it again under the workspace lock before publishing, so
/// a launcher publishing after the sweep would have had to see a clear flag
/// that no longer existed.
mod lifetime;
pub(crate) use lifetime::{Lifetime, Registration};

pub(crate) mod observation;

/// Maximum publication lock wait; this is not a shutdown grace timer.
pub const DEFAULT_PUBLISH_BOUND: Duration = Duration::from_secs(30);

pub(crate) struct Entry {
    root: PathBuf,
    workspace: Mutex<Workspace>,
    interested: Mutex<BTreeSet<AgentId>>,
    /// The workspace's slots, copied so the launch and demand paths read
    /// and move lifecycle state without the workspace lock. The
    /// workspace's own list grows when `refresh` lays out new slots, and
    /// [`Entry::sync_slots`] copies the growth here — in the same
    /// workspace-lock critical section that produced it, so an index into
    /// one is an index into the other and a new slot is reachable by
    /// demand, not by the workspace lock alone.
    slots: Mutex<Vec<Arc<ServerSlot>>>,
    /// Shared with the host and every other entry: the shutdown manager, and
    /// the flag that stops launches.
    lifetime: Lifetime,
    /// The same clock the workspace's slots start their idle clocks on, and
    /// what the sweep reads (`docs/observation.md`).
    clock: Arc<dyn Clock>,
    /// How long a slot may sit without a strong signal before the sweep
    /// stops it: the workspace's `idle_eviction_secs`, or the default.
    /// Held here so the sweep's park arithmetic never needs the workspace
    /// lock.
    idle_eviction: Duration,
    /// Closes the workspace's observation source. The observer thread ends
    /// when its source ends, so this is eager teardown at release: a
    /// released workspace stops watching its tree at once rather than at
    /// its next event (`docs/observation.md`). `None` when no observer was
    /// spawned — the `Session::new` construction path has none.
    observer_stop: Mutex<Option<Arc<dyn observation::Stop>>>,
}

impl Entry {
    pub(crate) fn new(workspace: Workspace, lifetime: Lifetime, clock: Arc<dyn Clock>) -> Self {
        let root = workspace.root().to_path_buf();
        let idle_eviction = workspace.idle_eviction();
        let slots = Mutex::new(workspace.slots().to_vec());
        Self {
            root,
            workspace: Mutex::new(workspace),
            interested: Mutex::new(BTreeSet::new()),
            slots,
            lifetime,
            clock,
            idle_eviction,
            observer_stop: Mutex::new(None),
        }
    }

    /// Lock the workspace for one operation. A panic while holding the lock
    /// poisons it, but the workspace's own invariants are re-established by
    /// the next `sync_owned`, so the poison is ignored rather than failing
    /// every later call.
    /// The workspace lock, or `None` if it is still held when `bound` passes.
    ///
    /// `std::sync::Mutex` offers no timed acquisition, so this polls. Polling
    /// is the honest shape here rather than an accident: the thing being
    /// bounded is another thread's critical section, which we cannot be
    /// notified about.
    pub(crate) fn lock_within(&self, bound: Duration) -> Option<MutexGuard<'_, Workspace>> {
        let deadline = Instant::now() + bound;
        loop {
            match self.workspace.try_lock() {
                Ok(workspace) => return Some(workspace),
                // The workspace's own invariants are re-established by the next
                // sync, so poisoned is a state we ignore rather than fail on.
                Err(TryLockError::Poisoned(poisoned)) => return Some(poisoned.into_inner()),
                Err(TryLockError::WouldBlock) => {}
            }
            if Instant::now() >= deadline {
                return None;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    pub(crate) fn admit(&self) -> Result<swansong::Guard> {
        self.lifetime.admit()
    }

    pub(crate) fn lock(&self) -> MutexGuard<'_, Workspace> {
        self.workspace
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn interested(&self) -> MutexGuard<'_, BTreeSet<AgentId>> {
        self.interested
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    /// Grow this entry's copy of the workspace's slots to match. Called by
    /// the observer in the same workspace-lock critical section as the
    /// `refresh` that appended, so every index `refresh` returned names
    /// the same slot here — the copy the demand path reads without the
    /// workspace lock, and that a later bind's warm walks in full.
    pub(crate) fn sync_slots(&self, slots: &[Arc<ServerSlot>]) {
        *self.slots.lock().unwrap_or_else(PoisonError::into_inner) = slots.to_vec();
    }

    /// The entry's slots, copied out: enough for a `serving` routing
    /// decision, which reads names and languages and holds nothing.
    pub(crate) fn slots_snapshot(&self) -> Vec<Arc<ServerSlot>> {
        self.slots
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The slot at `index`. `None` only for an index invented rather than
    /// obtained from `serving` or from a `refresh` this entry was synced
    /// with.
    pub(crate) fn slot(&self, index: usize) -> Option<Arc<ServerSlot>> {
        self.slots
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(index)
            .cloned()
    }

    /// How long the observer may park before the sweep runs again, or
    /// `None` when nothing can expire — the next wake is a batch, however
    /// far off. Computed without the workspace lock: the interval rides on
    /// this entry and a slot's state and clock sit behind their own locks,
    /// so a long query never delays the park arithmetic.
    pub(crate) fn sweep_park(&self) -> Option<Duration> {
        let snapshot = self.slots_snapshot();
        let now = self.clock.now();
        let earliest = snapshot
            .iter()
            .filter(|slot| !matches!(slot.state(), SlotState::Cold))
            .filter_map(|slot| slot.last_use().checked_add(self.idle_eviction))
            .min()?;
        Some(earliest.saturating_duration_since(now))
    }

    /// Stop what the idle sweep has expired, and hand the stopped servers
    /// back so the caller drops them outside the workspace lock. The expiry
    /// pre-check runs without the workspace lock — a long query never
    /// delays the decision to look — and the stop itself takes the lock
    /// once, as any workspace mutation does.
    pub(crate) fn sweep_due(&self) -> Vec<LspServer> {
        let snapshot = self.slots_snapshot();
        let now = self.clock.now();
        let due = snapshot.iter().any(|slot| {
            !matches!(slot.state(), SlotState::Cold)
                && now.saturating_duration_since(slot.last_use()) >= self.idle_eviction
        });
        if !due {
            return Vec::new();
        }
        let mut workspace = self.lock();
        workspace.evict_idle(now)
    }

    /// Close this workspace's observation source; the observer thread ends
    /// when its source ends. Idempotent — the handle is taken on first use.
    pub(crate) fn stop_observation(&self) {
        if let Some(stop) = self
            .observer_stop
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        {
            stop.stop();
        }
    }

    /// Start watching this workspace's tree: the observation thread
    /// that reduces every batch to an evidence refresh and runs the
    /// idle sweep (`docs/observation.md`). The thread holds only a
    /// [`Weak`] to this entry, so a released workspace is not kept
    /// alive by its watcher, and the stop handle ends the watching at
    /// once on release rather than at the tree's next event. Called
    /// once, when the workspace is created — by the host for an
    /// agent's binding, and by a standalone session alike, so a
    /// workspace is observed whichever way it was reached.
    pub(crate) fn observe(
        self: &Arc<Self>,
        evidence: Arc<observation::Pending>,
        watched: Arc<observation::Pending>,
        factory: &observation::SourceFactory,
    ) {
        let (source, stop) = factory(evidence, watched);
        *self
            .observer_stop
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(stop);
        let weak = Arc::downgrade(self);
        let _ = std::thread::Builder::new()
            .name("structural-observation".to_string())
            .spawn(move || observation::observe(&weak, source));
    }
}

/// Electing a launcher, walking a plan's ladder, and publishing what
/// initializes — the rest of [`Entry`]'s behaviour.
mod launch;

impl Drop for Entry {
    /// A workspace that no registry and no session holds is gone: close its
    /// observation source, so its watcher and its thread end now rather
    /// than at the tree's next event (`docs/observation.md`). The servers
    /// die with the workspace itself — dropping them is what reaps their
    /// children.
    fn drop(&mut self) {
        self.stop_observation();
    }
}

/// The process's workspaces, keyed by canonical root.
///
/// `structural.toml` is loaded from the workspace root, so today the
/// canonical root also determines the effective server configuration and is
/// a sufficient key. It grows a configuration component when a configuration
/// can come from somewhere other than the root.
pub struct Host {
    workspaces: Mutex<BTreeMap<PathBuf, Arc<Entry>>>,
    /// Workspaces released from `workspaces` that a [`Session`] may still
    /// hold. Weak, so a release never extends a workspace's life; kept so
    /// [`Host::shutdown`] can stop servers that outlived their agents'
    /// interest — otherwise those children would live until the holding
    /// session happened to drop, which in a long-lived host may be never.
    released: Mutex<Vec<Weak<Entry>>>,
    completion: Mutex<()>,
    /// The host's own lifetime, cloned into every entry it hands out.
    lifetime: Lifetime,
    /// The clock residency runs on: cloned into every entry, so the whole
    /// host advances together in tests (`crate::clock`).
    clock: Arc<dyn Clock>,
    /// Where each workspace's events come from. A factory, not a source:
    /// every workspace gets its own. The default is the platform watcher;
    /// tests install a synthetic source and drive it
    /// (`docs/observation.md`).
    source_factory: Mutex<observation::SourceFactory>,
}

impl Default for Host {
    fn default() -> Self {
        Self::new()
    }
}

impl Host {
    pub fn new() -> Self {
        Self::with_publish_bound(DEFAULT_PUBLISH_BOUND)
    }

    /// A host whose launches give up on a busy workspace after `publish_bound`
    /// and stop what they started, rather than waiting for a lock a wedged
    /// query may hold indefinitely.
    pub fn with_publish_bound(publish_bound: Duration) -> Self {
        Self {
            workspaces: Mutex::new(BTreeMap::new()),
            released: Mutex::new(Vec::new()),
            completion: Mutex::new(()),
            lifetime: Lifetime::with_publish_bound(publish_bound),
            clock: Arc::new(SystemClock),
            source_factory: Mutex::new(observation::default_source_factory()),
        }
    }

    /// A host whose workspaces are observed through `factory` instead of
    /// the platform watcher. For tests: a synthetic source the test drives
    /// exercises the same path a kernel watcher would, with no timing of
    /// its own (`src/host/tests/observation.rs`).
    #[cfg(test)]
    pub(crate) fn with_observation(
        factory: observation::SourceFactory,
        clock: Arc<dyn Clock>,
    ) -> Self {
        let mut host = Self::with_publish_bound(DEFAULT_PUBLISH_BOUND);
        host.source_factory = Mutex::new(factory);
        host.clock = clock;
        host
    }

    /// Bind `agent` to the workspace at `cwd`, creating it on first use.
    ///
    /// Creating a workspace is cheap — it reads the configuration and lays
    /// out a slot per server — and the eager warm that follows happens
    /// *outside* the map lock, so one workspace's startup never queues
    /// another's work behind it. Two agents arriving at once still produce
    /// one launch per server, by the slot's own election rather than by this
    /// lock. Operations on an already-acquired workspace never take it.
    pub fn agent_started(&self, agent: AgentId, cwd: &Path) -> Session {
        let view = canonical_root(cwd);
        let key = workspace_key(&view);
        // Interest is recorded before the map lock is released, so a
        // concurrent `agent_ended` cannot catch an entry no agent has bound
        // to yet and release it out from under this caller.
        let mut workspaces = self.lock();
        let entry = match workspaces.get(&key) {
            Some(entry) => Arc::clone(entry),
            None => {
                // The two buffers every event this workspace's
                // watcher sees land in: the observation thread
                // coalesces and drains one for evidence, and the
                // query path drains the other at the query
                // boundary (`docs/correctness.md`).
                let evidence = Arc::new(observation::Pending::new());
                let watched = Arc::new(observation::Pending::new());
                let entry = Arc::new(Entry::new(
                    Workspace::new(
                        key.clone(),
                        self.lifetime.flag(),
                        Arc::clone(&self.clock),
                        Arc::clone(&watched),
                    ),
                    self.lifetime.clone(),
                    Arc::clone(&self.clock),
                ));
                workspaces.insert(key, Arc::clone(&entry));
                // The workspace watches its own tree: an event batch re-runs
                // evidence and selection, and the same thread's park runs
                // the idle sweep. The thread holds only a Weak, so a
                // released workspace is not kept alive by its watcher
                // (`docs/observation.md`); the stop handle lets a release
                // end the watching at once instead of at the next event.
                entry.observe(evidence, watched, &self.source_factory.lock().unwrap());
                entry
            }
        };
        entry.interested().insert(agent);
        drop(workspaces);
        // Warm every configured server, eagerly, so indexing begins before
        // the first query — but out here, where a launch holds no host lock,
        // and without waiting for any of it. Binding is not a query: the
        // first tool call waits for these launches through
        // `Session::acquire`, while a front end that waited here would owe
        // its client nothing until the slowest index was warm
        // (`docs/ownership.md`). Two agents on one root still produce one
        // launch, now by the slot's own election rather than by this lock,
        // and a failure is recorded on the slot for each call to report
        // rather than failing the binding.
        entry.warm(None);
        Session::bind(entry, view)
    }

    /// Release `agent`'s interest in whatever it was bound to. Interest is
    /// per agent, so releasing one leaves another's session — and the shared
    /// servers — untouched. When the last interest goes, the workspace is
    /// released; servers stop when the last session holding it drops.
    pub fn agent_ended(&self, agent: &AgentId) {
        let mut workspaces = self.lock();
        let released: Vec<(PathBuf, Arc<Entry>)> = workspaces
            .iter()
            .filter(|(_, entry)| {
                let mut interested = entry.interested();
                interested.remove(agent);
                interested.is_empty()
            })
            .map(|(key, entry)| (key.clone(), Arc::clone(entry)))
            .collect();
        if released.is_empty() {
            return;
        }
        let mut detached = self.detached();
        detached.retain(|weak| weak.strong_count() > 0);
        for (key, entry) in released {
            workspaces.remove(&key);
            // A released workspace keeps observing — evidence maintenance
            // and the idle sweep both — until its last session drops; a
            // session holding it may still query, and a query still
            // demand-starts what it needs (`docs/observation.md`). When the
            // entry itself dies, `Entry`'s drop closes the watcher at once
            // instead of at the tree's next event.
            // Weak on purpose: the session that still holds this keeps it
            // alive, and shutdown can still reach it without the registry
            // extending its life.
            detached.push(Arc::downgrade(&entry));
        }
    }

    /// Close admission and initiate best-effort graceful shutdown without waiting.
    /// Already admitted tool calls may finish. Use `wait_shutdown` to reclaim
    /// servers, and `force_shutdown` from another thread if grace has elapsed.
    pub fn request_shutdown(&self) {
        self.lifetime.stop();
    }

    /// Interrupt server work without taking any workspace/query lock.
    /// Does not kill the caller or interrupt its local filesystem writes.
    pub fn force_shutdown(&self) {
        self.lifetime.force();
    }

    pub fn is_shutting_down(&self) -> bool {
        self.lifetime.stopped()
    }

    /// Wait for admitted work and cleanup. Concurrent waiters share the same
    /// completed cleanup; force remains independently callable throughout.
    /// A caller holding a raw workspace guard must release it before waiting.
    pub fn wait_shutdown(&self) {
        self.request_shutdown();
        let _completion = self
            .completion
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        self.lifetime.song.shut_down().block();
        let entries: Vec<Arc<Entry>> = {
            let mut workspaces = self.lock();
            let live = std::mem::take(&mut *workspaces).into_values();
            let detached: Vec<_> = self
                .detached()
                .drain(..)
                .filter_map(|weak| weak.upgrade())
                .collect();
            live.chain(detached).collect()
        };
        for entry in entries {
            let stopped = entry.lock().take_all();
            drop(stopped);
        }
    }

    /// Convenience soft request plus completion wait.
    pub fn shutdown(&self) {
        self.wait_shutdown();
    }

    fn lock(&self) -> MutexGuard<'_, BTreeMap<PathBuf, Arc<Entry>>> {
        self.workspaces
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn detached(&self) -> MutexGuard<'_, Vec<Weak<Entry>>> {
        self.released.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests;
