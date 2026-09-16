//! One workspace: a canonical root and the language servers configured for it.
//!
//! A workspace is created by the [`crate::host::Host`], which keys it by its
//! canonical root and shares it between every agent or caller bound to it.
//! Building one is cheap and starts nothing: it reads `structural.toml` and
//! lays out one [`ServerSlot`] per configured adapter. Servers start on
//! demand, through the host entry — which is why a workspace can exist, and
//! answer routing questions, before anything is running.
//!
//! Concurrency: the host wraps this in a mutex, so document ownership is
//! serialized per workspace — the same guarantee `&mut State` gave the MCP
//! dispatcher — without serializing unrelated workspaces. Each server runs on
//! its own threads behind its handle.

use crate::config::Config;
use crate::lsp::adapter::language_for;
use crate::lsp::server::LspServer;
use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::sync::Arc;

mod slot;

pub use slot::{Demand, Launch, ServerSlot, SlotState};

pub struct Workspace {
    /// The canonical workspace root, fixed when the workspace is acquired.
    /// A tool's `path` is a scope inside it; it never changes a server's root.
    root: PathBuf,
    /// One per configured adapter, in configuration order, fixed when the
    /// workspace is built. The host entry holds the same handles, so a demand
    /// can read and move lifecycle state without this workspace's lock.
    slots: Vec<Arc<ServerSlot>>,
    /// The running server for each slot, indexed alike: `Some` exactly when
    /// that slot is `Running`. Both change together, under this workspace's
    /// lock, which is what makes the pairing safe to rely on.
    servers: Vec<Option<LspServer>>,
    /// A present but unusable `structural.toml`: nothing starts, and every
    /// call says why.
    config_error: Option<String>,
}

impl Workspace {
    /// Read `structural.toml` and lay out a slot per configured server.
    ///
    /// Nothing is started here: a launch goes through the host entry, which
    /// can run and wait for one without holding the workspace lock. The root
    /// is canonicalized so every scope/candidate check compares real paths,
    /// not symlink spellings; if that fails (root missing) the raw root is
    /// kept for the error.
    pub fn new(root: PathBuf) -> Self {
        let root = canonical_root(&root);
        let mut workspace = Self {
            root,
            slots: Vec::new(),
            servers: Vec::new(),
            config_error: None,
        };
        let adapters = match Config::load(&workspace.root).and_then(Config::adapters) {
            Ok(adapters) => adapters,
            Err(error) => {
                workspace.config_error = Some(format!("{error:#}"));
                return workspace;
            }
        };
        for adapter in adapters {
            workspace.slots.push(Arc::new(ServerSlot::new(adapter)));
            workspace.servers.push(None);
        }
        workspace
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn slots(&self) -> &[Arc<ServerSlot>] {
        &self.slots
    }

    pub fn config_error(&self) -> Option<&str> {
        self.config_error.as_deref()
    }

    /// The running server in `index`'s slot.
    ///
    /// Every index a caller can obtain comes from [`Workspace::select`] or
    /// [`Workspace::index_of`], both of which yield running slots only, so
    /// this is an error only when a caller invents an index.
    pub fn server(&self, index: usize) -> Result<&LspServer> {
        self.servers
            .get(index)
            .and_then(Option::as_ref)
            .with_context(|| self.no_such_server(index))
    }

    pub fn server_mut(&mut self, index: usize) -> Result<&mut LspServer> {
        // Checked first, because naming the slot in the error needs an
        // immutable look at `self` that a held `&mut` would forbid.
        if self.servers.get(index).and_then(Option::as_ref).is_none() {
            bail!("{}", self.no_such_server(index));
        }
        Ok(self
            .servers
            .get_mut(index)
            .and_then(Option::as_mut)
            .expect("checked immediately above"))
    }

    /// Put a freshly started server in its slot. The slot's *state* is the
    /// launcher's to move, through the [`Launch`] guard it already holds.
    pub fn publish(&mut self, index: usize, server: LspServer) {
        if let Some(slot) = self.servers.get_mut(index) {
            *slot = Some(server);
        }
    }

    /// The slot running the server named `name`, when one is running.
    pub fn index_of(&self, name: &str) -> Option<usize> {
        self.slots
            .iter()
            .position(|slot| slot.name() == name)
            .filter(|index| self.servers[*index].is_some())
    }

    /// The running slots a query should go to: the ones serving `scope`'s
    /// extension when the scope is a file of a known language, otherwise
    /// every running slot. An actionable error when none of them is running,
    /// naming what each configured server is doing instead.
    pub fn select(&self, scope: Option<&Path>) -> Result<Vec<usize>> {
        let candidates = serving(&self.slots, scope);
        if candidates.is_empty() {
            // A config that will not parse means nothing was configured, so
            // the answer is about the file rather than about this scope.
            if self.config_error.is_some() {
                bail!("{}", self.nothing_running());
            }
            bail!(
                "no configured language server serves `{}` files (configured: {})",
                scope
                    .and_then(Path::extension)
                    .unwrap_or_default()
                    .to_string_lossy(),
                self.names()
            );
        }
        let running = self.running_of(&candidates);
        if running.is_empty() {
            // A file names the servers that should have served it; a bare
            // scope has nothing to name but the whole configuration.
            if scope.is_some_and(|scope| scope.is_file()) {
                bail!("{}", self.servers_down(&candidates));
            }
            bail!("{}", self.nothing_running());
        }
        Ok(running)
    }

    /// Every running server, for the questions a view asks about what is
    /// currently offered — adapters answer for what is merely configured.
    pub fn running(&self) -> impl Iterator<Item = &LspServer> {
        self.servers.iter().flatten()
    }

    /// One line per running server, for result footers.
    pub fn descriptions(&self) -> String {
        self.servers
            .iter()
            .flatten()
            .map(LspServer::description)
            .collect::<Vec<_>>()
            .join(" · ")
    }

    /// Stop a slot's server and forget it, leaving the slot cold for a later
    /// demand. One operation, so shutdown and eviction cannot disagree about
    /// what stopping means — and so a session that outlives the host finds a
    /// cold slot rather than a handle to a server that is gone.
    pub fn stop(&mut self, index: usize) {
        if let Some(server) = self.servers.get_mut(index).and_then(Option::take) {
            drop(server);
        }
        if let Some(slot) = self.slots.get(index) {
            slot.mark_cold();
        }
    }

    /// Stop every server. Idempotent, and each server's drop does the same, so
    /// a workspace dropped without this still reaps its children.
    pub fn shutdown(&mut self) {
        for index in 0..self.slots.len() {
            self.stop(index);
        }
    }

    /// Which of `candidates` have a server, read from the workspace's own
    /// copy: while this lock is held, a server in `servers` *is* a running
    /// server. The slot's state is for readers that hold no workspace lock.
    fn running_of(&self, candidates: &[usize]) -> Vec<usize> {
        candidates
            .iter()
            .copied()
            .filter(|index| self.servers[*index].is_some())
            .collect()
    }

    fn names(&self) -> String {
        self.slots
            .iter()
            .map(|slot| slot.name())
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn no_such_server(&self, index: usize) -> String {
        match self.slots.get(index) {
            Some(slot) => format!(
                "{} is {}, so it cannot answer this query",
                slot.name(),
                slot.state().label()
            ),
            None => format!("no such language server slot: {index}"),
        }
    }

    /// What to say when the servers that would serve a file are not running:
    /// each one's state, and how to fix the ones that failed.
    fn servers_down(&self, candidates: &[usize]) -> String {
        let mut text = String::new();
        for &index in candidates {
            let slot = &self.slots[index];
            let state = slot.state();
            text.push_str(&format!("  {}: {}", slot.name(), state.label()));
            if let Some(reason) = state.reason() {
                text.push_str(&format!(
                    ": {reason}; to fix: {}",
                    slot.adapter().install_hint()
                ));
            }
            text.push('\n');
        }
        text
    }

    fn nothing_running(&self) -> String {
        let mut text = String::from("no language server is running.\n");
        if let Some(error) = &self.config_error {
            text.push_str(&format!("  structural.toml: {error}\n"));
        }
        for slot in &self.slots {
            let state = slot.state();
            text.push_str(&format!("  {}: {}", slot.name(), state.label()));
            if let Some(reason) = state.reason() {
                text.push_str(&format!(
                    "; to fix: {}",
                    slot.adapter().install_hint().as_str()
                ));
                text.push_str(&format!("\n    {reason}"));
            }
            text.push('\n');
        }
        text.push_str("Restart this server after installing or configuring one.");
        text
    }
}

/// The slots that would serve `scope`: the ones whose adapter knows the
/// file's language, or every slot when the scope names no file.
///
/// One rule, in one place, because two callers need it for different reasons
/// — a launch asks which servers to start, a query asks which to send to —
/// and they must never disagree about it.
pub fn serving(slots: &[Arc<ServerSlot>], scope: Option<&Path>) -> Vec<usize> {
    let all = || (0..slots.len()).collect::<Vec<usize>>();
    let Some(scope) = scope.filter(|scope| scope.is_file()) else {
        return all();
    };
    let serving: Vec<usize> = (0..slots.len())
        .filter(|index| language_for(&slots[*index].adapter().languages(), scope).is_some())
        .collect();
    if serving.is_empty() && scope.extension().is_none() {
        // A file of no known language and no extension leaves nothing to
        // route by, so every server is a candidate, as it always was.
        return all();
    }
    serving
}

/// The canonical spelling of a root, so two callers naming the same tree
/// through different paths — a symlink, a relative path, a trailing slash —
/// land on one workspace. A root that cannot be canonicalized (it does not
/// exist) is kept as given, for the error to report.
pub fn canonical_root(root: &Path) -> PathBuf {
    std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf())
}
