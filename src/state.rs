//! The MCP server's state: one fixed workspace root, one rust-analyzer
//! session, started eagerly at server start.
//!
//! Concurrency: mcplease dispatches tool calls on a single thread and hands
//! this state over as `&mut State`, so the session's document ownership is
//! serialized by construction. The server itself runs on its own threads
//! behind the session's handle.

use crate::lsp::session::Session;
use std::path::PathBuf;

pub struct State {
    /// The canonical workspace root, fixed at server start. `show`'s `path`
    /// is a scope inside it; it never changes the server's root.
    root: PathBuf,
    /// The live rust-analyzer session, or `None` with an actionable
    /// `startup_error` when the server could not be started at all. The MCP
    /// server must still come up and serve `tools/list` in that case; the
    /// tool reports the reason instead of panicking.
    session: Option<Session>,
    startup_error: Option<String>,
}

impl State {
    /// Start rust-analyzer eagerly. Failure to start is recorded, not fatal:
    /// the MCP surface answers with an actionable error per call. The root is
    /// canonicalized so every scope/candidate check compares real paths, not
    /// symlink spellings; if canonicalization fails (root missing), the
    /// session start below fails too and the raw root is kept for the error.
    pub fn new(root: PathBuf) -> Self {
        let root = std::fs::canonicalize(&root).unwrap_or(root);
        match Session::start(&root) {
            Ok(session) => Self {
                root,
                session: Some(session),
                startup_error: None,
            },
            Err(error) => Self {
                root,
                session: None,
                startup_error: Some(error.to_string()),
            },
        }
    }

    pub fn root(&self) -> &std::path::Path {
        &self.root
    }

    /// The session, or an actionable error when rust-analyzer is not running.
    pub fn session(&mut self) -> anyhow::Result<&mut Session> {
        match self.session.as_mut() {
            Some(session) => Ok(session),
            None => {
                let reason = self.startup_error.clone().unwrap_or_default();
                Err(anyhow::anyhow!(
                    "rust-analyzer is not running: {reason}. \
                     Install it (`rustup component add rust-analyzer`), ensure it is on PATH, \
                     and restart this server."
                ))
            }
        }
    }
}
