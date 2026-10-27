//! Binding an agent to a workspace, releasing it, and binding it again
//! elsewhere for one call.
//!
//! Interest is per agent and per workspace: binding records it, and
//! [`Host::agent_ended`] removes it wherever it was recorded. A tool call
//! with a `workspace` argument binds the same agent through the same host
//! to the workspace that directory belongs to ([`Rebind`]), so it shares
//! servers with every other caller on that root and is released by the
//! same idle sweep (`docs/ownership.md`, `docs/observation.md`). Nothing
//! here changes the process's working directory or any other global: a
//! binding is a [`Session`] value.

use super::{AgentId, Entry, Host, observation, workspace_key};
use crate::session::Session;
use crate::workspace::{Workspace, canonical_root};
use anyhow::{Result, bail};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Weak};

/// How a session bound through a host binds its caller elsewhere: the host
/// it was bound through, and as whom.
///
/// Weak, so a session that outlives its host — a tool call finishing
/// after shutdown — does not keep the host alive, and is told the host is
/// gone rather than given a workspace nothing will shut down.
#[derive(Clone)]
pub(crate) struct Rebind {
    host: Weak<Host>,
    agent: AgentId,
}

impl Rebind {
    /// The same caller, bound to the workspace at `cwd` for one call.
    pub(crate) fn bind(&self, cwd: &Path) -> Result<Session> {
        let Some(host) = self.host.upgrade() else {
            bail!("the host has shut down");
        };
        // Admission is refused anyway once shutdown has begun; refusing
        // here as well keeps a workspace from being created after the
        // shutdown sweep has already taken the map.
        if host.is_shutting_down() {
            bail!("the host is shutting down");
        }
        let (entry, view, newly) = host.attach(self.agent.clone(), cwd);
        // Warm on the agent's first binding here only. Every later call
        // routes its own scope through `Session::acquire`, which touches
        // the slots it addresses; warming everything on every call would
        // refresh every idle clock in the workspace and keep the idle
        // sweep from ever releasing a server no call asks for
        // (`docs/observation.md`).
        if newly {
            entry.warm(None, None);
        }
        Ok(Session::bind(entry, view).rebinding(self.clone()))
    }
}

impl Host {
    /// Bind `agent` to the workspace at `cwd`, as [`Host::agent_started`]
    /// does, on a session that can bind the same agent elsewhere through
    /// this host for one call — the `workspace` argument every tool takes.
    /// The MCP front end binds this way.
    pub fn bind(self: &Arc<Self>, agent: AgentId, cwd: &Path) -> Session {
        let rebind = Rebind {
            host: Arc::downgrade(self),
            agent: agent.clone(),
        };
        self.agent_started(agent, cwd).rebinding(rebind)
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
        let (entry, view, _) = self.attach(agent, cwd);
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
        entry.warm(None, None);
        Session::bind(entry, view)
    }

    /// Record `agent`'s interest in the workspace at `cwd`, creating the
    /// workspace on first use, and say whether the interest is new. Starts
    /// nothing: whether to warm is the caller's.
    fn attach(&self, agent: AgentId, cwd: &Path) -> (Arc<Entry>, PathBuf, bool) {
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
                        &self.config_source,
                        self.lifetime.flag(),
                        Arc::clone(&self.clock),
                        Arc::clone(&watched),
                    ),
                    self.lifetime.clone(),
                    Arc::clone(&self.clock),
                    Arc::clone(&self.operator),
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
        let newly = entry.interested().insert(agent);
        drop(workspaces);
        (entry, view, newly)
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
}
