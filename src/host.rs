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

use crate::lsp::server::LspServer;
use crate::session::Session;
use crate::workspace::{Demand, Launch, ServerSlot, Workspace, canonical_root};
use anyhow::{Result, bail};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, TryLockError, Weak};
use std::time::{Duration, Instant};

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

/// Maximum publication lock wait; this is not a shutdown grace timer.
pub const DEFAULT_PUBLISH_BOUND: Duration = Duration::from_secs(30);

pub(crate) struct Entry {
    root: PathBuf,
    workspace: Mutex<Workspace>,
    interested: Mutex<BTreeSet<AgentId>>,
    slots: Vec<Arc<ServerSlot>>,
    /// Shared with the host and every other entry: the shutdown manager, and
    /// the flag that stops launches.
    lifetime: Lifetime,
}

impl Entry {
    pub(crate) fn new(workspace: Workspace, lifetime: Lifetime) -> Self {
        let root = workspace.root().to_path_buf();
        let slots = workspace.slots().to_vec();
        Self {
            root,
            workspace: Mutex::new(workspace),
            interested: Mutex::new(BTreeSet::new()),
            slots,
            lifetime,
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

    /// Make the servers that would answer for `scope` running, starting the
    /// ones that are not.
    ///
    /// No workspace lock is held here, deliberately: a launch publishes
    /// *under* that lock, so waiting for one while holding it would deadlock
    /// against the very launcher this call elects.
    pub(crate) fn activate(self: &Arc<Self>, scope: Option<&Path>) {
        let wanted = crate::workspace::serving(&self.slots, scope);
        // Start every needed launch first and wait afterwards. Waiting as we
        // went would serialize exactly what giving each launch a thread was
        // for: one server's `initialize` would still be a delay to the next.
        for &index in &wanted {
            // Best effort: a server that will not start is recorded on its
            // own slot, and the call goes on to ask for what is available.
            // Bailing here would let one broken server stop every other from
            // being warmed at all.
            let _ = self.start(index);
        }
        for &index in &wanted {
            let _ = self.slots[index].wait_for_launch();
        }
    }

    /// Bring one slot to `Running`, or say why it cannot be.
    ///
    /// A demand that finds the slot already starting joins that launch rather
    /// than beginning a second one, which is what makes concurrent demand
    /// produce exactly one process.
    fn start(self: &Arc<Self>, index: usize) -> Result<()> {
        let guard = self.lifetime.admit()?;
        match self.slots[index].demand() {
            // Already running, or someone else's launch is under way; either
            // way the wait in `activate` covers it.
            Demand::Ready | Demand::Join => Ok(()),
            Demand::Failed(reason) => bail!("{reason}"),
            Demand::Refused => bail!("the host is shutting down"),
            Demand::Lead(launch) => {
                self.spawn_launch(index, launch, guard);
                Ok(())
            }
        }
    }

    /// Start one server and publish it.
    ///
    /// The spawn and the `initialize` handshake hold no lock at all, so other
    /// workspaces — and this one's other slots — keep working while it runs.
    /// Only publication takes the workspace lock, and it takes it once, so no
    /// caller can observe a `Running` slot whose server is not yet in place.
    fn spawn_launch(self: &Arc<Self>, index: usize, launch: Launch, guard: swansong::Guard) {
        let entry = Arc::clone(self);
        // A launch owns its thread, so two servers initialize at once instead
        // of one waiting out the other's whole handshake. The guard is taken
        // here and returned by a drop *inside* the thread, so it comes back on
        // every exit path — including a panic, which an explicit "finished"
        // call would never reach. A launch that dies must still let a shutdown
        // finish; a launch counter said the same thing until it turned out a
        // panicking launcher could leak it.
        // A thread that could not be created takes the closure, and the guard
        // inside it, with the error — so there is nothing to undo here. The
        // `Launch` guard inside has already marked the slot failed and woken
        // anyone waiting on it.
        let _ = std::thread::Builder::new()
            .name(format!("structural-launch-{index}"))
            .spawn(move || {
                let _guard = guard;
                entry.launch(index, launch);
            });
    }

    /// One launch, on its own thread. Spawning and `initialize` hold no lock;
    /// publication takes the workspace lock once, so no caller can observe a
    /// `Running` slot whose server is not yet in place.
    ///
    /// Failures go to the slot, which the `Launch` guard does on every exit
    /// path including a panic — so a waiter learns why rather than hanging.
    fn launch(&self, index: usize, launch: Launch) {
        if self.lifetime.stopped() {
            return launch.failed("the host is shutting down");
        }
        let spawning = match LspServer::spawn(launch.adapter_arc(), &self.root) {
            Ok(spawning) => spawning,
            Err(error) => return launch.failed(format!("{error:#}")),
        };
        // Reachable by a shutdown from here on, though no workspace holds it
        // yet: this is the window in which a child would otherwise be
        // orphaned by a host that goes away mid-handshake.
        let Some(child) = self.lifetime.register(spawning.stopper()) else {
            // The host began stopping between the check above and this
            // registration, so nothing will stop this child but us. Dropping
            // the half-started server tears it down and reaps it.
            drop(spawning);
            return launch.failed("the host is shutting down");
        };
        let mut server = match spawning.initialize() {
            Ok(server) => server,
            Err(error) => return launch.failed(format!("{error:#}")),
        };
        // The workspace lock, or nothing if it stays held past the bound. A
        // query holding this lock is unbounded in real time by design, so
        // waiting for it would make a shutdown wait with it.
        let Some(mut workspace) = self.lock_within(self.lifetime.publish_bound) else {
            drop(server);
            return launch.failed(format!(
                "this workspace was busy for {:?} while the server was starting, so it was \
                 stopped rather than left running",
                self.lifetime.publish_bound
            ));
        };
        if self.lifetime.stopped() {
            // Shutdown claimed the host while this server was starting. Drop it
            // rather than publish one nobody will look after — outside the
            // workspace lock, because dropping it reaps a child.
            drop(workspace);
            drop(server);
            return launch.failed("the host shut down while this server was starting");
        }
        child.published();
        server.set_registration(child);
        workspace.publish(index, server);
        launch.published();
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
        }
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
        let key = canonical_root(cwd);
        // Interest is recorded before the map lock is released, so a
        // concurrent `agent_ended` cannot catch an entry no agent has bound
        // to yet and release it out from under this caller.
        let mut workspaces = self.lock();
        let entry = match workspaces.get(&key) {
            Some(entry) => Arc::clone(entry),
            None => {
                let entry = Arc::new(Entry::new(
                    Workspace::new(key.clone(), self.lifetime.flag()),
                    self.lifetime.clone(),
                ));
                workspaces.insert(key, Arc::clone(&entry));
                entry
            }
        };
        entry.interested().insert(agent);
        drop(workspaces);
        // Warm every configured server, eagerly, so indexing begins before
        // the first query — but out here, where a launch holds no host lock.
        // Two agents on one root still produce one launch, now by the slot's
        // own election rather than by this lock, and a failure is recorded on
        // the slot for each call to report rather than failing the binding.
        entry.activate(None);
        Session::bind(entry)
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
