//! A caller's binding to one workspace.
//!
//! This is what a tool is handed: mcplease gives each call a `&mut Session`
//! and the caller's context, and the tool takes the workspace guard for the
//! duration of the call. Two callers bound to the same root share one
//! workspace — and so one set of language-server processes — while two
//! bound to different roots lock independently.

use crate::host::Entry;
use crate::workspace::{Workspace, canonical_root};
use std::path::PathBuf;
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
        Self::bind(Arc::new(Entry::new(Workspace::new(canonical_root(&root)))))
    }

    pub(crate) fn bind(workspace: Arc<Entry>) -> Self {
        Self { workspace }
    }

    /// Lock the workspace for one operation. Serialized against other calls
    /// on the same workspace, and against nothing else.
    pub fn workspace(&self) -> MutexGuard<'_, Workspace> {
        self.workspace.lock()
    }
}
