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

use crate::lsp::handle::Stopper;
use crate::lsp::server::LspServer;
use crate::session::Session;
use crate::workspace::{Demand, Launch, ServerSlot, Workspace, canonical_root};
use anyhow::{Result, bail};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use swansong::Swansong;

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
#[derive(Clone)]
pub(crate) struct Lifetime {
    song: Swansong,
    stopped: Arc<AtomicBool>,
}

impl Lifetime {
    pub(crate) fn new() -> Self {
        Self {
            song: Swansong::new(),
            stopped: Arc::new(AtomicBool::new(false)),
        }
    }

    /// The shared claim itself, for the slots a workspace lays out. Shared
    /// rather than copied: a copy per slot would be a second claim, and two
    /// claims can disagree.
    pub(crate) fn flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.stopped)
    }

    fn stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }

    /// No launch may begin, and shutdown is *initiated* — one operation,
    /// because the two are one decision and separating them is what let a
    /// caller wait for a completion that had never been started. Waiting for
    /// completion without stopping waits forever; the pair existed once and
    /// cost an afternoon.
    fn stop(&self) {
        self.stopped.store(true, Ordering::SeqCst);
        // The completion future is dropped on purpose: the shutdown state
        // lives in the swansong, and the wait is taken separately, after the
        // children have been stopped. Clippy is right that `let _ =` on a
        // future is the wrong way to say that.
        drop(self.song.shut_down());
    }
}

pub(crate) struct Entry {
    root: PathBuf,
    workspace: Mutex<Workspace>,
    interested: Mutex<BTreeSet<AgentId>>,
    slots: Vec<Arc<ServerSlot>>,
    /// Children that are being started right now, which no workspace holds
    /// yet. A shutdown kills these rather than waiting for the threads
    /// initializing them — which is what keeps its exit bounded by teardown
    /// instead of by a hung handshake.
    children: Mutex<BTreeMap<u64, Stopper>>,
    next_child: AtomicU64,
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
            children: Mutex::new(BTreeMap::new()),
            next_child: AtomicU64::new(0),
            lifetime,
        }
    }

    /// Lock the workspace for one operation. A panic while holding the lock
    /// poisons it, but the workspace's own invariants are re-established by
    /// the next `sync_owned`, so the poison is ignored rather than failing
    /// every later call.
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
        match self.slots[index].demand() {
            // Already running, or someone else's launch is under way; either
            // way the wait in `activate` covers it.
            Demand::Ready | Demand::Join => Ok(()),
            Demand::Failed(reason) => bail!("{reason}"),
            Demand::Refused => bail!("the host is shutting down"),
            Demand::Lead(launch) => {
                self.spawn_launch(index, launch);
                Ok(())
            }
        }
    }

    /// Take responsibility for a child while it is being started. The guard
    /// gives the registration back on every exit path, including a panic.
    fn register_child(&self, stopper: Stopper) -> Option<ChildGuard<'_>> {
        let mut children = self.children();
        // Refusing once the lifetime is stopping is what keeps the sweep
        // exhaustive. The claim is set before the sweep collects, so every
        // child that *did* register is visible to that collection; without
        // this check a child could register just after it, have nobody to stop
        // it, and hold shutdown open for a whole handshake — measured, not
        // assumed: a guard taken after a stop keeps completion pending.
        //
        // The check must stay *inside* this lock. Reading the flag first and
        // then taking the lock reopens the window it closes: the collector
        // could run between the two. Registration and collection are one
        // mutex, and that is the whole argument.
        if self.lifetime.stopped() {
            return None;
        }
        let id = self.next_child.fetch_add(1, Ordering::SeqCst);
        children.insert(id, stopper);
        Some(ChildGuard { entry: self, id })
    }

    fn children(&self) -> MutexGuard<'_, BTreeMap<u64, Stopper>> {
        self.children.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Stop every child that is still being started. Their launchers then
    /// fail, and publish nothing.
    fn stop_starting_children(&self) {
        let stoppers: Vec<Stopper> = self.children().values().cloned().collect();
        for stopper in stoppers {
            stopper.stop();
        }
    }

    /// Start one server and publish it.
    ///
    /// The spawn and the `initialize` handshake hold no lock at all, so other
    /// workspaces — and this one's other slots — keep working while it runs.
    /// Only publication takes the workspace lock, and it takes it once, so no
    /// caller can observe a `Running` slot whose server is not yet in place.
    fn spawn_launch(self: &Arc<Self>, index: usize, launch: Launch) {
        let entry = Arc::clone(self);
        // A launch owns its thread, so two servers initialize at once instead
        // of one waiting out the other's whole handshake. The guard is taken
        // here and returned by a drop *inside* the thread, so it comes back on
        // every exit path — including a panic, which an explicit "finished"
        // call would never reach. A launch that dies must still let a shutdown
        // finish; a launch counter said the same thing until it turned out a
        // panicking launcher could leak it.
        let guard = self.lifetime.song.guard();
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
        let Some(_child) = self.register_child(spawning.stopper()) else {
            // The host began stopping between the check above and this
            // registration, so nothing will stop this child but us. Dropping
            // the half-started server tears it down and reaps it.
            drop(spawning);
            return launch.failed("the host is shutting down");
        };
        let server = match spawning.initialize() {
            Ok(server) => server,
            Err(error) => return launch.failed(format!("{error:#}")),
        };
        let mut workspace = self.lock();
        if self.lifetime.stopped() {
            // Shutdown claimed the host while this server was starting. Drop
            // it rather than publish one nobody will look after — outside the
            // workspace lock, because dropping it reaps a child.
            drop(workspace);
            drop(server);
            return launch.failed("the host shut down while this server was starting");
        }
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
        Self {
            workspaces: Mutex::new(BTreeMap::new()),
            released: Mutex::new(Vec::new()),
            lifetime: Lifetime::new(),
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

    /// Stop every workspace's servers, so no child outlives the host —
    /// including a workspace whose agents released it while a session still
    /// holds it, which the map no longer names. Any session still held
    /// becomes unusable; the embedded consumer owns that ordering, and the
    /// MCP process does this on the way out.
    pub fn shutdown(&self) {
        let mut workspaces = self.lock();
        let live: Vec<Arc<Entry>> = std::mem::take(&mut *workspaces).into_values().collect();
        let detached: Vec<Arc<Entry>> = self
            .detached()
            .drain(..)
            .filter_map(|weak| weak.upgrade())
            .collect();
        let entries: Vec<Arc<Entry>> = live.into_iter().chain(detached).collect();
        // The order is what makes this exhaustive. Claiming the lifetime
        // first means a launcher still inside `initialize` cannot publish into
        // a workspace the sweep has already passed — and, unlike a claim per
        // slot, it also covers a workspace acquired from here on. Stopping the
        // starting children reaches the ones no workspace holds yet. Then the
        // wait: every launch holds a guard, and this returns when the last of
        // them has given it back, which is the same "every decision has been
        // made" the launch counter expressed — except a guard cannot be
        // skipped by a panic, and the counter could. The sweep last, so every
        // child is either published and stopped here, or killed by its
        // launcher.
        self.lifetime.stop();
        for entry in &entries {
            entry.stop_starting_children();
        }
        // `stop` has already initiated; `shut_down().block()` is idempotent on
        // the initiation and does the waiting, so this call is self-contained
        // even though the wait depends on that earlier initiation.
        //
        // This wait is also what makes the sweep below *final*: a launch
        // returns its guard only after it has finished writing to its slot, so
        // no slot write can land after a `mark_cold`. Bounding this wait with
        // a timeout would lose that quietly — the state would still be swept,
        // and a later writer could resurrect it.
        self.lifetime.song.shut_down().block();
        for entry in &entries {
            // Taken under the lock, reaped outside it: dropping a server joins
            // its threads, and this is the lock a launcher needs to publish.
            let stopped = entry.lock().take_all();
            drop(stopped);
        }
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

/// Holds a child's registration for as long as its launch is in flight, and
/// gives it back on every exit path, including a panic.
struct ChildGuard<'a> {
    entry: &'a Entry,
    id: u64,
}

impl Drop for ChildGuard<'_> {
    fn drop(&mut self) {
        self.entry.children().remove(&self.id);
    }
}

#[cfg(test)]
mod tests;
