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
use crate::workspace::{Workspace, canonical_root};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};

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
pub(crate) struct Entry {
    workspace: Mutex<Workspace>,
    interested: Mutex<BTreeSet<AgentId>>,
}

impl Entry {
    pub(crate) fn new(workspace: Workspace) -> Self {
        Self {
            workspace: Mutex::new(workspace),
            interested: Mutex::new(BTreeSet::new()),
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
        }
    }

    /// Bind `agent` to the workspace at `cwd`, creating it on first use.
    ///
    /// A new workspace starts its servers here, eagerly, so indexing begins
    /// before the first query. The map lock is held across that startup,
    /// which makes concurrent demand for one workspace produce one launch —
    /// this is the simple form; unit 2 replaces it with an acquisition that
    /// does not hold the global lock while a server initializes. Operations
    /// on an already-acquired workspace never take this lock.
    pub fn agent_started(&self, agent: AgentId, cwd: &Path) -> Session {
        let key = canonical_root(cwd);
        // Interest is recorded before the map lock is released, so a
        // concurrent `agent_ended` cannot catch an entry no agent has bound
        // to yet and release it out from under this caller.
        let mut workspaces = self.lock();
        let entry = match workspaces.get(&key) {
            Some(entry) => Arc::clone(entry),
            None => {
                let entry = Arc::new(Entry::new(Workspace::new(key.clone())));
                workspaces.insert(key, Arc::clone(&entry));
                entry
            }
        };
        entry.interested().insert(agent);
        drop(workspaces);
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
        for entry in live.into_iter().chain(detached) {
            entry.lock().shutdown();
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

#[cfg(test)]
mod tests;
