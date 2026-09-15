//! One workspace: a canonical root and the language servers running for it.
//!
//! A workspace is created by the [`crate::host::Host`], which keys it by its
//! canonical root and shares it between every agent or caller bound to it.
//! Servers start eagerly at creation so indexing begins immediately.
//!
//! Concurrency: the host wraps this in a mutex, so document ownership is
//! serialized per workspace — the same guarantee `&mut State` gave the MCP
//! dispatcher — without serializing unrelated workspaces. Each server runs on
//! its own threads behind its handle.

use crate::config::Config;
use crate::lsp::adapter::language_for;
use crate::lsp::server::LspServer;
use anyhow::{Result, bail};
use std::path::{Path, PathBuf};

/// A configured server that did not start.
pub struct Unavailable {
    pub name: String,
    pub reason: String,
    pub hint: String,
}

pub struct Workspace {
    /// The canonical workspace root, fixed when the workspace is acquired.
    /// A tool's `path` is a scope inside it; it never changes a server's root.
    root: PathBuf,
    servers: Vec<LspServer>,
    unavailable: Vec<Unavailable>,
    /// A present but unusable `structural.toml`: nothing starts, and every
    /// call says why.
    config_error: Option<String>,
}

impl Workspace {
    /// Load `structural.toml` and start every configured server whose
    /// command can be spawned. Failure to start any one of them is recorded,
    /// not fatal: the MCP surface must still come up and serve `tools/list`,
    /// and each call reports what is missing. The root is canonicalized so
    /// every scope/candidate check compares real paths, not symlink
    /// spellings; if that fails (root missing) the raw root is kept for the
    /// error.
    pub fn new(root: PathBuf) -> Self {
        let root = canonical_root(&root);
        let mut workspace = Self {
            root,
            servers: Vec::new(),
            unavailable: Vec::new(),
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
            let name = adapter.name().to_string();
            let hint = adapter.install_hint();
            match LspServer::start(adapter, &workspace.root) {
                Ok(server) => workspace.servers.push(server),
                Err(error) => workspace.unavailable.push(Unavailable {
                    name,
                    reason: format!("{error:#}"),
                    hint,
                }),
            }
        }
        workspace
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn servers(&self) -> &[LspServer] {
        &self.servers
    }

    pub fn server(&self, index: usize) -> &LspServer {
        &self.servers[index]
    }

    pub fn server_mut(&mut self, index: usize) -> &mut LspServer {
        &mut self.servers[index]
    }

    pub fn index_of(&self, name: &str) -> Option<usize> {
        self.servers.iter().position(|server| server.name() == name)
    }

    pub fn unavailable(&self) -> &[Unavailable] {
        &self.unavailable
    }

    /// The servers a query should go to: the one serving `scope`'s
    /// extension when the scope is a file of a known language, otherwise
    /// every running server. An actionable error when none is running.
    pub fn select(&self, scope: Option<&Path>) -> Result<Vec<usize>> {
        if self.servers.is_empty() {
            bail!("{}", self.nothing_running());
        }
        if let Some(scope) = scope
            && scope.is_file()
        {
            let serving: Vec<usize> = self
                .servers
                .iter()
                .enumerate()
                .filter(|(_, server)| language_for(&server.adapter().languages(), scope).is_some())
                .map(|(index, _)| index)
                .collect();
            if !serving.is_empty() {
                return Ok(serving);
            }
            if scope.extension().is_some() {
                bail!(
                    "no running language server serves `{}` files (running: {})",
                    scope.extension().unwrap_or_default().to_string_lossy(),
                    self.servers
                        .iter()
                        .map(LspServer::name)
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
        }
        Ok((0..self.servers.len()).collect())
    }

    /// One line per running server, for result footers.
    pub fn descriptions(&self) -> String {
        self.servers
            .iter()
            .map(LspServer::description)
            .collect::<Vec<_>>()
            .join(" · ")
    }

    /// Stop every server. Idempotent, and each server's drop does the same, so
    /// a workspace dropped without this still reaps its children.
    pub fn shutdown(&mut self) {
        for server in &mut self.servers {
            server.shutdown();
        }
    }

    fn nothing_running(&self) -> String {
        let mut text = String::from("no language server is running.\n");
        if let Some(error) = &self.config_error {
            text.push_str(&format!("  structural.toml: {error}\n"));
        }
        for unavailable in &self.unavailable {
            text.push_str(&format!(
                "  {}: {}; to fix: {}\n",
                unavailable.name, unavailable.reason, unavailable.hint
            ));
        }
        text.push_str("Restart this server after installing or configuring one.");
        text
    }
}

/// The canonical spelling of a root, so two callers naming the same tree
/// through different paths — a symlink, a relative path, a trailing slash —
/// land on one workspace. A root that cannot be canonicalized (it does not
/// exist) is kept as given, for the error to report.
pub fn canonical_root(root: &Path) -> PathBuf {
    std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf())
}
