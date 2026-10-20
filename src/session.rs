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
//!
//! A call that names another directory in `workspace` runs in a session
//! of its own for that call ([`Session::elsewhere`]): bound through the
//! same host as the same agent when this session was bound through one,
//! otherwise a fresh [`Session::new`]. Nothing about this session changes,
//! so another caller's default — and this caller's next call — is
//! untouched (`docs/ownership.md`).

use crate::host::{Entry, Rebind};
use crate::workspace::{Workspace, canonical_root};
use anyhow::{Result, bail};
use std::path::{Path, PathBuf};
use std::sync::{Arc, MutexGuard};

#[derive(Clone)]
pub struct Session {
    workspace: Arc<Entry>,
    /// The caller's working directory: what `path` is relative to, what
    /// answers are filtered against, and what paths are shown relative
    /// to. Always inside the workspace root, or equal to it.
    view: PathBuf,
    /// How to bind this caller elsewhere through the host it was bound
    /// through; `None` for a session built by [`Session::new`], which has
    /// no host.
    rebind: Option<Rebind>,
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
            Arc::new(crate::operator::Stderr),
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
        Self {
            workspace,
            view,
            rebind: None,
        }
    }

    /// This session, able to bind its caller elsewhere through a host.
    pub(crate) fn rebinding(mut self, rebind: Rebind) -> Self {
        self.rebind = Some(rebind);
        self
    }

    /// The session one call runs in: this one, or — when the call names a
    /// `workspace` — the caller bound to the project that directory
    /// belongs to, with that directory as its view.
    ///
    /// `workspace` is absolute or relative to this session's view, and must
    /// be a directory. Its workspace is keyed exactly as at binding
    /// ([`crate::host::workspace_key`]), so a subdirectory of a project
    /// lands on that project's servers and answers about the
    /// subdirectory. A directory under this session's own key reuses this
    /// session's workspace with the new view. Otherwise a session bound
    /// through a host binds again through it as the same agent — sharing
    /// that root's servers with every other caller on it, its interest
    /// lasting until the agent ends — and one built by [`Session::new`]
    /// gets a fresh `Session::new` for the call.
    pub fn elsewhere(&self, workspace: Option<&str>) -> Result<Session> {
        let Some(named) = workspace.map(str::trim).filter(|named| !named.is_empty()) else {
            return Ok(self.clone());
        };
        let joined = self.view.join(named);
        let Ok(dir) = std::fs::canonicalize(&joined) else {
            bail!(
                "`workspace` names `{named}`, which does not exist (from {})",
                self.view.display()
            );
        };
        if !dir.is_dir() {
            bail!("`workspace` names `{named}`, which is a file, not a directory");
        }
        if dir == self.view {
            return Ok(self.clone());
        }
        if crate::host::workspace_key(&dir) == self.workspace.root() {
            return Ok(Self {
                workspace: Arc::clone(&self.workspace),
                view: dir,
                rebind: self.rebind.clone(),
            });
        }
        match &self.rebind {
            Some(rebind) => rebind.bind(&dir),
            None => Ok(Session::new(dir)),
        }
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
        self.workspace.activate(scope, None)
    }

    /// Start the servers that would answer for `scope` without waiting for
    /// them. For binding a session, where nothing has been asked yet;
    /// [`Session::activate`] — which every query goes through — is what
    /// waits (`docs/ownership.md`).
    pub fn warm(&self, scope: Option<&Path>) {
        self.workspace.warm(scope, None);
    }

    /// Take the workspace for one call, starting whatever this call's scope
    /// and language need first.
    ///
    /// `path` is a tool's own relative scope, resolved against the view
    /// here, and `language` the tool's narrowing, so that starting servers
    /// and querying them route through the same rule
    /// ([`crate::workspace::serving`]). Keeping the ordering inside one
    /// function is the point: a launch publishes *under* the workspace
    /// lock, so a call that took the lock and then waited for a launch
    /// would be waiting for itself. A `language` that no server here
    /// speaks, or that a file `path` contradicts, is an error before
    /// anything is asked ([`Workspace::check_language`]).
    pub fn acquire(&self, path: Option<&str>, language: Option<&str>) -> Result<Operation<'_>> {
        let admission = self.workspace.admit()?;
        // Against the view, not the workspace root: an agent in
        // `crates/alpha` asking for `path: "src"` means its own `src`.
        let scope = path.map(|path| self.view.join(path));
        self.workspace.activate(scope.as_deref(), language);
        let workspace = self.workspace();
        workspace.check_language(scope.as_deref(), language)?;
        Ok(Operation {
            workspace,
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
