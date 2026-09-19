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

use crate::session::Session;
use crate::workspace::{ServerSlot, Workspace, canonical_root};
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
}

/// Electing a launcher, walking a plan's ladder, and publishing what
/// initializes — the rest of [`Entry`]'s behaviour.
mod launch;

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
        let view = canonical_root(cwd);
        let key = workspace_key(&view);
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
