//! The rust-analyzer session used by the MCP `show` slice.
//!
//! One server for one fixed workspace root, started eagerly at MCP-server
//! start (`Session::start` from [`crate::state::State::new`]) on its own
//! threads ([`ServerHandle`]). Readiness is "no outstanding work": no open
//! progress token, and rust-analyzer's own quiescence signal agreeing
//! ([`rust_analyzer::Quiescence`]). A query that arrives before that blocks
//! until it holds; the only way out is the inactivity backstop, which means
//! the server is broken, not busy.
//!
//! Freshness comes from message ordering, not timing: the session *owns*
//! every file it has touched, pushes the on-disk text with `didOpen` /
//! `didChange` before querying, and the server must apply those before it
//! handles any request that follows them on the wire. No settle waits, no
//! re-queries.

use anyhow::{Context, Result, bail};
use lsp_types::{Position, SymbolInformation};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::lsp::adapter::rust_analyzer;
use crate::lsp::handle::{ServerHandle, Settings, Snapshot};

/// The inactivity backstop for every wait and query: the server must say
/// *something* this often or it is treated as broken. Per-server
/// configuration arrives with the registry (slice 2, item 3).
pub const BACKSTOP: Duration = Duration::from_secs(90);

/// A document the session has opened, with the snapshot the server was last
/// given.
struct OpenedDoc {
    version: i32,
    text: String,
}

/// What [`Session::sync`] pushed: the document's URI, the on-disk text the
/// server now holds, and whether anything had to be sent.
#[derive(Debug)]
pub struct Synced {
    pub uri: String,
    pub text: String,
    /// `true` when a `didOpen` or `didChange` went out; `false` when the
    /// server's copy already matched disk.
    pub fed: bool,
}

pub struct Session {
    root: PathBuf,
    /// `None` only after [`Session::shutdown`].
    handle: Option<ServerHandle>,
    /// `serverInfo` from `initialize`, when the server reports one.
    server_info: Option<Value>,
    /// The position encoding the server negotiated at `initialize` (LSP
    /// default is UTF-16 when absent).
    position_encoding: Option<String>,
    /// Documents opened with the server, keyed by URI, with the snapshot the
    /// server was last given.
    opened: HashMap<String, OpenedDoc>,
}

impl Session {
    /// Spawn rust-analyzer and initialize it against `root`. Returns once
    /// `initialize` is answered; indexing continues in the background and
    /// the first query waits for it.
    pub fn start(root: &Path) -> Result<Self> {
        let settings = Settings {
            backstop: BACKSTOP,
            ..Settings::default()
        };
        let handle = ServerHandle::spawn_with(
            rust_analyzer::COMMAND,
            &[],
            settings,
            Box::new(rust_analyzer::Quiescence::default()),
        )?;

        let params = json!({
            "processId": std::process::id(),
            "clientInfo": { "name": "structural", "version": env!("CARGO_PKG_VERSION") },
            "rootUri": path_uri(root)?,
            "rootPath": root.to_string_lossy(),
            "workspaceFolders": [{ "uri": path_uri(root)?, "name": "workspace" }],
            "trace": "off",
            "initializationOptions": {},
            "capabilities": {
                "workspace": {
                    "configuration": true,
                    "symbol": { "symbolKind": { "valueSet": (1..=26).collect::<Vec<_>>() } }
                },
                "textDocument": {
                    "synchronization": {
                        "dynamicRegistration": false,
                        // Full-text changes: `sync` sends the whole file
                        // when the disk copy differs.
                        "didSave": true,
                        "change": 1
                    },
                    "definition": { "linkSupport": false },
                    "references": {},
                    "documentSymbol": { "hierarchicalDocumentSymbolSupport": true },
                    "hover": { "contentFormat": ["markdown", "plaintext"] },
                    "codeAction": {
                        "dynamicRegistration": false,
                        "dataSupport": true,
                        "resolveSupport": { "properties": ["edit"] },
                        "codeActionLiteralSupport": {
                            "codeActionKind": {
                                "valueSet": ["", "quickfix", "refactor", "refactor.extract", "refactor.inline", "refactor.rewrite", "source", "source.organizeImports"]
                            }
                        }
                    }
                },
                "window": { "workDoneProgress": true, "showMessage": {} },
                "general": { "positionEncodings": ["utf-16"] },
                "experimental": rust_analyzer::experimental_capabilities(),
            }
        });

        let result = handle
            .request("initialize", params)
            .context("initializing rust-analyzer")?;
        let server_info = result.get("serverInfo").cloned();
        let position_encoding = result
            .pointer("/capabilities/positionEncoding")
            .and_then(Value::as_str)
            .map(String::from);
        handle.notify("initialized", json!({}))?;

        Ok(Self {
            root: root.to_path_buf(),
            handle: Some(handle),
            server_info,
            position_encoding,
            opened: HashMap::new(),
        })
    }

    fn handle(&self) -> &ServerHandle {
        self.handle
            .as_ref()
            .expect("the handle is present until shutdown")
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn server_info(&self) -> Option<&Value> {
        self.server_info.as_ref()
    }

    pub fn position_encoding(&self) -> Option<&str> {
        self.position_encoding.as_deref()
    }

    /// Block until the server has no outstanding work. Errors only when the
    /// server has been silent for [`BACKSTOP`] or the connection failed.
    pub fn wait_ready(&self) -> Result<()> {
        self.handle().wait_ready(BACKSTOP)
    }

    /// Routine connection metadata (ready flag, open tokens, last message).
    pub fn snapshot(&self) -> Result<Snapshot> {
        self.handle().snapshot()
    }

    /// The diagnostic event log: notifications, server requests, outgoing
    /// frames. For tests and debugging; never consulted for readiness.
    pub fn events(&self) -> Result<Vec<Value>> {
        self.handle().capture_events()
    }

    /// Push the on-disk text of every document the session owns, so the next
    /// query runs against the server's current view of them. Returns how
    /// many documents had to be (re)fed.
    pub fn sync_owned(&mut self) -> Result<usize> {
        let paths: Vec<PathBuf> = self
            .opened
            .keys()
            .filter_map(|uri| uri_file_path(uri))
            .collect();
        let mut fed = 0usize;
        for path in paths {
            fed += self
                .sync(&path)
                .map(|synced| usize::from(synced.fed))
                .map_err(|error| error.context("syncing an owned document before the query"))?;
        }
        Ok(fed)
    }

    /// Make the server's copy of `path` match disk: `didOpen` on first
    /// touch, a full-text `didChange` when the file changed since the last
    /// snapshot, nothing when equal. Returns the on-disk text just read —
    /// the same bytes the server now holds, so callers convert positions
    /// against them.
    ///
    /// A previously opened file that is now missing or unreadable is an
    /// actionable error; its snapshot is discarded and the server is told the
    /// document closed, so no later call keeps querying a ghost.
    pub fn sync(&mut self, path: &Path) -> Result<Synced> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) => {
                // Without the didClose the server would keep its snapshot (a
                // ghost symbol a later query could still resolve) and a
                // recreated file would hit its duplicate-didOpen guard.
                let was_opened = match path_uri(path) {
                    Ok(uri) => {
                        if self.opened.remove(&uri).is_some() {
                            let _ = self.handle().notify(
                                "textDocument/didClose",
                                json!({ "textDocument": { "uri": uri } }),
                            );
                            true
                        } else {
                            false
                        }
                    }
                    Err(_) => false,
                };
                let context = if was_opened {
                    format!(
                        "{} was opened earlier but is now missing or unreadable on disk; its \
                         stale snapshot was discarded — re-check the file and retry",
                        path.display()
                    )
                } else {
                    format!("reading {} to sync the server's copy", path.display())
                };
                return Err(error).context(context);
            }
        };
        let uri = path_uri(path)?;
        let fed = match self.opened.get(&uri) {
            Some(doc) if doc.text == text => false,
            Some(doc) => {
                let version = doc.version + 1;
                self.handle().notify(
                    "textDocument/didChange",
                    json!({
                        "textDocument": { "uri": uri, "version": version },
                        "contentChanges": [ { "text": text } ],
                    }),
                )?;
                // `textDocument/didSave` belongs here once a diagnostics view
                // exists: it triggers rust-analyzer's check-on-save, which
                // nothing in this slice consumes.
                self.opened.insert(
                    uri.clone(),
                    OpenedDoc {
                        version,
                        text: text.clone(),
                    },
                );
                true
            }
            None => {
                self.handle().notify(
                    "textDocument/didOpen",
                    json!({
                        "textDocument": {
                            "uri": uri,
                            "languageId": "rust",
                            "version": 1,
                            "text": text,
                        }
                    }),
                )?;
                self.opened.insert(
                    uri.clone(),
                    OpenedDoc {
                        version: 1,
                        text: text.clone(),
                    },
                );
                true
            }
        };
        Ok(Synced { uri, text, fed })
    }

    /// `workspace/symbol` query, decoded into candidates as the server
    /// reported them (no completeness claim is made anywhere downstream).
    pub fn workspace_symbol(&self, query: &str) -> Result<Vec<SymbolInformation>> {
        self.check_encoding()?;
        let result = self
            .handle()
            .request("workspace/symbol", json!({ "query": query }))?;
        let infos: Option<Vec<SymbolInformation>> =
            serde_json::from_value(result).context("parsing workspace/symbol result")?;
        Ok(infos.unwrap_or_default())
    }

    /// `textDocument/definition` at a position; the raw result is normalized
    /// by [`crate::lsp::definition::parse_definition_result`].
    pub fn definition(&self, uri: &str, position: Position) -> Result<Value> {
        self.check_encoding()?;
        self.handle().request(
            "textDocument/definition",
            json!({
                "textDocument": { "uri": uri },
                "position": position,
            }),
        )
    }

    /// The negotiated position encoding must be UTF-16: this slice's
    /// conversions are UTF-16 and the server was asked for UTF-16. A server
    /// that negotiated otherwise is reported, not silently mishandled.
    fn check_encoding(&self) -> Result<()> {
        match self.position_encoding.as_deref() {
            None | Some("utf-16") => Ok(()),
            Some(other) => bail!(
                "server negotiated `{other}` position encoding; this slice supports utf-16 only"
            ),
        }
    }

    /// Shut the server down (graceful, then kill). Idempotent.
    pub fn shutdown(&mut self) {
        if let Some(handle) = self.handle.take() {
            let _ = handle.shutdown();
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// The filesystem path behind a `file:` URI string, for iterating the
/// session's opened documents.
fn uri_file_path(uri: &str) -> Option<PathBuf> {
    url::Url::parse(uri).ok()?.to_file_path().ok()
}

/// The `file:` URI of a canonical path.
pub fn path_uri(path: &Path) -> Result<String> {
    url::Url::from_file_path(path)
        .map(|url| url.to_string())
        .map_err(|_| anyhow::anyhow!("path {} is not absolute", path.display()))
}

#[cfg(test)]
mod tests;
