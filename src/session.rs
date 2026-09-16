//! A caller's binding to one workspace.
//!
//! This is what a tool is handed: mcplease gives each call a `&mut Session`
//! and the caller's context, and the tool takes the workspace guard for the
//! duration of the call. Two callers bound to the same root share one
//! workspace — and so one set of language-server processes — while two
//! bound to different roots lock independently.
//!
//! Starting servers is deliberately *not* part of taking that guard:
//! [`Session::activate`] runs first, holding no workspace lock, because a
//! launch publishes under the guard and would otherwise wait on the caller
//! that is waiting for it.

use crate::host::Entry;
use crate::workspace::{Workspace, canonical_root};
use std::path::{Path, PathBuf};
use std::sync::{Arc, MutexGuard};

pub struct Session {
    workspace: Arc<Entry>,
}

impl Session {
    /// A session for `root` on a workspace of its own, for the command-line
    /// path and in-process tests. No agent is registered as interested, so
    /// nothing releases it but dropping it. Front ends that share workspaces
    /// go through [`crate::host::Host::agent_started`] instead.
    pub fn new(root: PathBuf) -> Self {
        let lifetime = crate::host::Lifetime::new();
        let session = Self::bind(Arc::new(Entry::new(
            Workspace::new(canonical_root(&root), lifetime.flag()),
            lifetime,
        )));
        // Warm every configured server, as the MCP process does when an agent
        // starts. A server that will not start is recorded on its slot, for
        // each call to report, rather than failing the session.
        session.activate(None);
        session
    }

    pub(crate) fn bind(workspace: Arc<Entry>) -> Self {
        Self { workspace }
    }

    /// Make the servers that would answer for `scope` running, starting the
    /// ones that are not. Call this *before* taking [`Session::workspace`].
    ///
    /// Best effort: a server that will not start is recorded on its own slot
    /// and the call goes on to ask for what is available. What is missing is
    /// reported by the query, which knows the scope.
    pub fn activate(&self, scope: Option<&Path>) {
        self.workspace.activate(scope)
    }

    /// Take the workspace for one call, starting whatever this call's scope
    /// needs first.
    ///
    /// `path` is a tool's own relative scope, resolved against the root here
    /// so that starting servers and querying them agree about which servers a
    /// scope needs. Keeping the ordering inside one function is the point: a
    /// launch publishes *under* the workspace lock, so a call that took the
    /// lock and then waited for a launch would be waiting for itself.
    pub fn acquire(&self, path: Option<&str>) -> MutexGuard<'_, Workspace> {
        let scope = {
            let workspace = self.workspace();
            path.map(|path| workspace.root().join(path))
        };
        self.activate(scope.as_deref());
        self.workspace()
    }

    /// Lock the workspace for one operation. Serialized against other calls
    /// on the same workspace, and against nothing else.
    pub fn workspace(&self) -> MutexGuard<'_, Workspace> {
        self.workspace.lock()
    }
}
