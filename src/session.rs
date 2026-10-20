//! A caller's binding to one workspace.
//!
//! This is what a tool is handed: mcplease gives each call a clone of the
//! `Session` and the caller's context, and the tool takes the workspace guard
//! for the duration of the call. Two callers bound to the same root share one
//! workspace — and so one set of language-server processes — while two
//! bound to different roots lock independently.
//!
//! Starting servers is deliberately *not* part of taking that guard:
//! [`Session::activate`] runs first, holding no workspace lock, because a
//! launch publishes under the guard and would otherwise wait on the caller
//! that is waiting for it.
//!
//! A session also carries the caller's **view**: the working directory it
//! was bound with. The workspace is keyed by the project root above that
//! directory, so two agents in sibling members of one cargo workspace
//! share servers — but each is still asking about its own member, so
//! `path` resolves against the view, answers are filtered against it, and
//! paths are displayed relative to it. The key and the filter used to be
//! the same field; separating them is what lets sharing widen without
//! answers widening (`docs/discovery.md`).

use crate::host::Entry;
use crate::workspace::{Workspace, canonical_root};
use std::path::{Path, PathBuf};
use std::sync::{Arc, MutexGuard};

#[derive(Clone)]
pub struct Session {
    workspace: Arc<Entry>,
    /// The caller's working directory: what `path` is relative to, what
    /// answers are filtered against, and what paths are shown relative
    /// to. Always inside the workspace root, or equal to it.
    view: PathBuf,
}

impl Session {
    /// A session for `root` on a workspace of its own, for the command-line
    /// path and in-process tests. No agent is registered as interested, so
    /// nothing releases it but dropping it. Front ends that share workspaces
    /// go through [`crate::host::Host::agent_started`] instead.
    pub fn new(cwd: PathBuf) -> Self {
        let lifetime = crate::host::Lifetime::new();
        let view = canonical_root(&cwd);
        // The two buffers every event this workspace's watcher
        // sees land in, and the watching that fills them: a session
        // built this way is held to the same freshness boundary
        // as one bound through the host (`docs/correctness.md`).
        let evidence = Arc::new(crate::host::observation::Pending::new());
        let watched = Arc::new(crate::host::observation::Pending::new());
        let entry = Arc::new(Entry::new(
            Workspace::new(
                crate::host::workspace_key(&view),
                lifetime.flag(),
                Arc::new(crate::clock::SystemClock),
                Arc::clone(&watched),
            ),
            lifetime,
            Arc::new(crate::clock::SystemClock),
        ));
        entry.observe(
            evidence,
            watched,
            &crate::host::observation::default_source_factory(),
        );
        let session = Self::bind(entry, view);
        // Warm every configured server, as the MCP process does when an agent
        // starts, and do not wait for them: the query that follows waits. A
        // server that will not start is recorded on its slot, for each call
        // to report, rather than failing the session.
        session.warm(None);
        session
    }

    pub(crate) fn bind(workspace: Arc<Entry>, view: PathBuf) -> Self {
        Self { workspace, view }
    }

    /// The caller's working directory. Every user-facing path — a `path`
    /// scope, a filter, a rendered location — is relative to this, never
    /// to the workspace key.
    pub fn view(&self) -> &Path {
        &self.view
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

    /// Start the servers that would answer for `scope` without waiting for
    /// them. For binding a session, where nothing has been asked yet;
    /// [`Session::activate`] — which every query goes through — is what
    /// waits (`docs/ownership.md`).
    pub fn warm(&self, scope: Option<&Path>) {
        self.workspace.warm(scope);
    }

    /// Take the workspace for one call, starting whatever this call's scope
    /// needs first.
    ///
    /// `path` is a tool's own relative scope, resolved against the root here
    /// so that starting servers and querying them agree about which servers a
    /// scope needs. Keeping the ordering inside one function is the point: a
    /// launch publishes *under* the workspace lock, so a call that took the
    /// lock and then waited for a launch would be waiting for itself.
    pub fn acquire(&self, path: Option<&str>) -> anyhow::Result<Operation<'_>> {
        let admission = self.workspace.admit()?;
        // Against the view, not the workspace root: an agent in
        // `crates/alpha` asking for `path: "src"` means its own `src`.
        let scope = path.map(|path| self.view.join(path));
        self.activate(scope.as_deref());
        Ok(Operation {
            workspace: self.workspace(),
            view: self.view.clone(),
            _admission: admission,
        })
    }

    /// Lock the workspace for one operation. Serialized against other calls
    /// on the same workspace, and against nothing else.
    pub fn workspace(&self) -> MutexGuard<'_, Workspace> {
        let _waiting = crate::progress::waiting_on("waiting for another query on this workspace");
        self.workspace.lock()
    }
}

/// Whole-call protection: release the workspace before returning admission.
pub struct Operation<'a> {
    workspace: MutexGuard<'a, Workspace>,
    view: PathBuf,
    _admission: swansong::Guard,
}

impl Operation<'_> {
    /// The caller's working directory, carried in from the session so a
    /// tool never has to reach for the workspace key to answer about
    /// paths. Shadows nothing: [`Workspace::root`] remains the key, and
    /// the two differ whenever an agent works inside a larger project.
    pub fn view(&self) -> &Path {
        &self.view
    }
}
impl std::ops::Deref for Operation<'_> {
    type Target = Workspace;
    fn deref(&self) -> &Workspace {
        &self.workspace
    }
}
impl std::ops::DerefMut for Operation<'_> {
    fn deref_mut(&mut self) -> &mut Workspace {
        &mut self.workspace
    }
}
