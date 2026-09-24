//! One running language server of any kind: spawn, initialize, own
//! documents, query, shut down. Everything server-specific comes from its
//! [`Adapter`].
//!
//! Readiness is "no outstanding work": no open progress token, and the
//! adapter's readiness policy agreeing. A query that arrives before that
//! blocks until it holds; the only way out is the inactivity backstop, which
//! means the server is broken, not busy.
//!
//! Freshness comes from message ordering, not timing: the server *owns*
//! every file it has touched, pushes the on-disk text with `didOpen` /
//! `didChange` before querying, and the language server must apply those
//! before it handles any request that follows them on the wire. No settle
//! waits, no re-queries.

use anyhow::{Context, Result, bail};
use lsp_types::{Position, SymbolInformation};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::lsp::adapter::{Adapter, language_for};
use crate::lsp::handle::{ServerHandle, Settings, Snapshot, Stopper};
use crate::lsp::positions::Encoding;

/// A document the server has been given, with the snapshot it was last sent.
struct OpenedDoc {
    version: i32,
    text: String,
}

/// What [`LspServer::sync`] pushed: the document's URI, the on-disk text the
/// server now holds, and whether anything had to be sent.
#[derive(Debug)]
pub struct Synced {
    pub uri: String,
    pub text: String,
    /// `true` when a `didOpen` or `didChange` went out; `false` when the
    /// server's copy already matched disk.
    pub fed: bool,
}

/// A server process that has been started but has not answered `initialize`.
///
/// The handshake is the slow part — minutes for rust-analyzer on a large
/// project — and a host has to be able to stop the process while it is inside
/// it, which it cannot do through a handle it does not hold yet. That is what
/// [`Spawning::stopper`] is for.
pub struct Spawning {
    adapter: Arc<dyn Adapter>,
    root: PathBuf,
    handle: ServerHandle,
}

impl Spawning {
    /// Enough to stop this child without owning it.
    pub fn stopper(&self) -> Stopper {
        self.handle.stopper()
    }

    pub fn name(&self) -> &str {
        self.adapter.name()
    }

    /// Finish the handshake and take ownership of the connection.
    pub fn initialize(self) -> Result<LspServer> {
        let Self {
            adapter,
            root,
            handle,
        } = self;
        let params = json!({
            "processId": std::process::id(),
            "clientInfo": { "name": "structural", "version": env!("CARGO_PKG_VERSION") },
            "rootUri": path_uri(&root)?,
            "rootPath": root.to_string_lossy(),
            "workspaceFolders": [{ "uri": path_uri(&root)?, "name": "workspace" }],
            "trace": "off",
            "initializationOptions": adapter.initialization_options(),
            "capabilities": adapter.client_capabilities(),
        });
        let result = handle
            .request("initialize", params)
            .with_context(|| format!("initializing {}", adapter.name()))?;
        let server_info = result.get("serverInfo").cloned();
        let encoding = match result
            .pointer("/capabilities/positionEncoding")
            .and_then(Value::as_str)
        {
            None | Some("utf-16") => Encoding::Utf16,
            Some("utf-8") => Encoding::Utf8,
            Some(other) => bail!(
                "{} negotiated the `{other}` position encoding, which this client does not \
                 convert (offered: {})",
                adapter.name(),
                adapter.position_encodings().join(", ")
            ),
        };
        let capabilities = result.get("capabilities").cloned().unwrap_or(Value::Null);
        handle.notify("initialized", json!({}))?;

        Ok(LspServer {
            adapter,
            root,
            handle: Some(handle),
            server_info,
            encoding,
            capabilities,
            opened: HashMap::new(),
            registration: None,
        })
    }
}

pub struct LspServer {
    adapter: Arc<dyn Adapter>,
    root: PathBuf,
    /// `None` only after [`LspServer::shutdown`].
    handle: Option<ServerHandle>,
    /// `serverInfo` from `initialize`, when the server reports one.
    server_info: Option<Value>,
    /// The position encoding the server picked from the adapter's offer.
    encoding: Encoding,
    /// The server's capabilities as returned by `initialize`.
    capabilities: Value,
    /// Documents opened with the server, keyed by URI, with the snapshot the
    /// server was last given.
    opened: HashMap<String, OpenedDoc>,
    registration: Option<crate::host::Registration>,
}

impl LspServer {
    pub(crate) fn set_registration(&mut self, registration: crate::host::Registration) {
        self.registration = Some(registration);
    }

    /// Spawn the adapter's command, and stop there.
    ///
    /// The process is running and nothing has been sent yet: `initialize` is
    /// the next step, and keeping it separate is what lets a caller hold —
    /// and stop — a child that is still handshaking. Most callers want
    /// [`LspServer::start`], which does both.
    pub fn spawn(adapter: Arc<dyn Adapter>, root: &Path) -> Result<Spawning> {
        let argv = adapter.command();
        let (command, args) = argv
            .split_first()
            .context("an adapter's command must name an executable")?;
        let settings = Settings {
            // Answered to `workspace/configuration`, which a server may
            // send during startup and before any caller exists.
            configuration: adapter.workspace_configuration(),
            backstop: adapter.backstop(),
            progress_create_is_work: adapter.progress_create_is_work(),
            ..Settings::default()
        };
        let handle = ServerHandle::spawn_with(command, args, settings, adapter.readiness_policy())
            .with_context(|| format!("starting {}", adapter.name()))?;
        Ok(Spawning {
            adapter,
            root: root.to_path_buf(),
            handle,
        })
    }

    /// Spawn the adapter's command and initialize it against `root`. Returns
    /// once `initialize` is answered; indexing continues in the background
    /// and the first query waits for it.
    ///
    /// Spawning and `initialize` are separable — see [`LspServer::spawn`] —
    /// because a host has to be able to stop a child that is still
    /// handshaking, and the handshake is the slow part.
    pub fn start(adapter: Arc<dyn Adapter>, root: &Path) -> Result<Self> {
        Self::spawn(adapter, root)?.initialize()
    }

    fn handle(&self) -> &ServerHandle {
        self.handle
            .as_ref()
            .expect("the handle is present until shutdown")
    }

    pub fn adapter(&self) -> &dyn Adapter {
        self.adapter.as_ref()
    }

    pub fn name(&self) -> &str {
        self.adapter.name()
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn encoding(&self) -> Encoding {
        self.encoding
    }

    pub fn server_info(&self) -> Option<&Value> {
        self.server_info.as_ref()
    }

    /// `name version` as the server reported it at `initialize`, or the
    /// adapter's name when it reported nothing (pyright does not).
    pub fn description(&self) -> String {
        let reported = self.server_info.as_ref().and_then(|info| {
            let name = info.get("name").and_then(Value::as_str)?;
            let version = info.get("version").and_then(Value::as_str).unwrap_or("?");
            Some(format!("{name} {version}"))
        });
        reported.unwrap_or_else(|| format!("{} (serverInfo not reported)", self.name()))
    }

    /// Block until the server has no outstanding work. Errors only when the
    /// server has been silent for the adapter's backstop or the connection
    /// failed.
    pub fn wait_ready(&self) -> Result<()> {
        let _waiting =
            crate::progress::waiting_on(format!("waiting for {} to finish indexing", self.name()));
        self.handle().wait_ready(self.adapter.backstop())
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

    /// Open one file this server serves, so its symbol index is populated.
    ///
    /// Seven of nineteen servers measured answer `workspace/symbol` with
    /// nothing until some document is open, and answer the identical query
    /// once one is (`reference/lsp-server-conventions.md`). Without this a
    /// bare name — the most common query shape — is answered `no symbol
    /// named 'x'` on a fresh session about a symbol plainly in the tree.
    ///
    /// **Any** file warms it: opening one file makes symbols defined in
    /// files never opened resolve. So this is one file read per server and
    /// not a guess about where the answer is (`server/warm.rs`).
    ///
    /// Done once, as part of becoming ready, and never as a retry after an
    /// empty answer — re-asking a query that missed is the retry loop the
    /// contract forbids (`docs/correctness.md`). A server with no file of
    /// its language in the tree warms nothing, which is not an error, and a
    /// file that cannot be read is left to the query that names it.
    pub fn warm_index(&mut self) -> Option<PathBuf> {
        if !self.supports("workspaceSymbolProvider") {
            return None;
        }
        let file = warm::first_served_file(&self.root.clone(), self.adapter.as_ref())?;
        self.sync(&file).ok()?;
        // Opening it is not enough for every server. pyright schedules the
        // analysis an open implies and populates its workspace index from
        // it, so a bare `didOpen` followed immediately by a query still
        // races: the index is empty until something makes the server look
        // at the document. One request against it does, and it is the
        // request an editor sends on open anyway.
        if let Ok(uri) = path_uri(&file) {
            let _ = self.document_symbol(&uri);
        }
        // Then the same wait every query does — for the server to have no
        // outstanding work, bounded only by the inactivity backstop
        // (docs/correctness.md). Not a settle sleep.
        self.wait_ready().ok()?;
        Some(file)
    }

    /// Push the on-disk text of every document this server owns, so the
    /// next query runs against its current view of them. Returns how many
    /// documents had to be (re)fed.
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
                let language_id = self.language_id(path);
                self.handle().notify(
                    "textDocument/didOpen",
                    json!({
                        "textDocument": {
                            "uri": uri,
                            "languageId": language_id,
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

    /// The LSP language id for `path` per the adapter, falling back to the
    /// adapter's first language: a file the server itself pointed at is one
    /// it serves.
    pub fn language_id(&self, path: &Path) -> String {
        let languages = self.adapter.languages();
        language_for(&languages, path)
            .map(|language| language.id.clone())
            .or_else(|| languages.first().map(|language| language.id.clone()))
            .unwrap_or_else(|| "plaintext".to_string())
    }

    /// `workspace/symbol`, with the adapter's query rewriting, decoded into
    /// candidates as the server reported them (no completeness claim is
    /// made anywhere downstream).
    ///
    /// Empty when the server did not advertise the capability, the same way
    /// [`Self::document_symbol`] answers `None`. Sending it anyway earns a
    /// `-32601`, and that error used to end the whole query: pylsp 1.15.0
    /// serves documents but has no symbol index, and a *file-scoped* query
    /// that never needed the index failed on it. A server that cannot
    /// nominate from an index contributes no candidates; the other servers
    /// still do, and the file's own outline still does.
    pub fn workspace_symbol(&self, query: &str) -> Result<Vec<SymbolInformation>> {
        if !self.supports("workspaceSymbolProvider") {
            return Ok(Vec::new());
        }
        let query = self.adapter.symbol_query(query);
        let result = self
            .handle()
            .request("workspace/symbol", json!({ "query": query }))?;
        let infos: Option<Vec<SymbolInformation>> =
            serde_json::from_value(result).context("parsing workspace/symbol result")?;
        Ok(infos.unwrap_or_default())
    }

    /// `textDocument/definition` at a position (in this server's encoding);
    /// the raw result is normalized by
    /// [`crate::lsp::definition::parse_definition_result`].
    pub fn definition(&self, uri: &str, position: Position) -> Result<Value> {
        self.handle().request(
            "textDocument/definition",
            json!({
                "textDocument": { "uri": uri },
                "position": position,
            }),
        )
    }

    /// `textDocument/references` at a position (in this server's encoding),
    /// without the declaration; the raw result is normalized by
    /// [`crate::lsp::references::parse_references_result`].
    pub fn references(&self, uri: &str, position: Position) -> Result<Value> {
        self.handle().request(
            "textDocument/references",
            json!({
                "textDocument": { "uri": uri },
                "position": position,
                "context": { "includeDeclaration": false },
            }),
        )
    }

    /// The server's capabilities as returned by `initialize`.
    pub fn capabilities(&self) -> &Value {
        &self.capabilities
    }

    /// Whether the server advertised a capability: `provider` is the
    /// `ServerCapabilities` key (`documentSymbolProvider`,
    /// `callHierarchyProvider`, …); absent, `null` and `false` are no.
    pub fn supports(&self, provider: &str) -> bool {
        !matches!(
            self.capabilities.get(provider),
            None | Some(Value::Null) | Some(Value::Bool(false))
        )
    }

    /// Any request, for adapter-defined extension views; the standard
    /// queries have their own methods.
    pub fn request(&self, method: &str, params: Value) -> Result<Value> {
        self.handle().request(method, params)
    }

    /// `textDocument/implementation` at a position; a `Location[]` (or
    /// link) result in the same shapes as `definition`.
    pub fn implementation(&self, uri: &str, position: Position) -> Result<Value> {
        self.handle().request(
            "textDocument/implementation",
            json!({ "textDocument": { "uri": uri }, "position": position }),
        )
    }

    /// `textDocument/prepareCallHierarchy` at a position: the
    /// `CallHierarchyItem[]` the direction queries take.
    pub fn prepare_call_hierarchy(&self, uri: &str, position: Position) -> Result<Value> {
        self.handle().request(
            "textDocument/prepareCallHierarchy",
            json!({ "textDocument": { "uri": uri }, "position": position }),
        )
    }

    /// `callHierarchy/incomingCalls` or `outgoingCalls` for one item.
    pub fn calls(&self, incoming: bool, item: &Value) -> Result<Value> {
        let method = if incoming {
            "callHierarchy/incomingCalls"
        } else {
            "callHierarchy/outgoingCalls"
        };
        self.handle().request(method, json!({ "item": item }))
    }

    /// `textDocument/hover` over `range`: at its start, or over the whole
    /// range when the adapter says the server takes one.
    pub fn hover(&self, uri: &str, range: lsp_types::Range) -> Result<Value> {
        self.handle()
            .request("textDocument/hover", self.adapter.hover_params(uri, range))
    }

    /// `textDocument/prepareRename` at a position: `null` when the server
    /// says nothing there can be renamed.
    pub fn prepare_rename(&self, uri: &str, position: Position) -> Result<Value> {
        self.handle().request(
            "textDocument/prepareRename",
            json!({ "textDocument": { "uri": uri }, "position": position }),
        )
    }

    /// `textDocument/rename`: the `WorkspaceEdit` that renames the symbol
    /// at a position, as a result to apply here, never applied by the
    /// server.
    pub fn rename(&self, uri: &str, position: Position, new_name: &str) -> Result<Value> {
        self.handle().request(
            "textDocument/rename",
            json!({ "textDocument": { "uri": uri }, "position": position, "newName": new_name }),
        )
    }

    /// A file this server owns was moved on disk: the server is told the
    /// old document closed, and forgets it; the next `sync` of the new
    /// path opens it. A file that was not owned needs nothing.
    pub fn moved(&mut self, from: &Path) -> Result<()> {
        let uri = path_uri(from)?;
        if self.opened.remove(&uri).is_some() {
            self.handle().notify(
                "textDocument/didClose",
                json!({ "textDocument": { "uri": uri } }),
            )?;
        }
        Ok(())
    }

    /// `textDocument/documentSymbol` for a document, or `None` when the
    /// server did not advertise the capability. The raw result is read by
    /// [`crate::lsp::outline::innermost_at`].
    pub fn document_symbol(&self, uri: &str) -> Result<Option<Value>> {
        if !self.supports("documentSymbolProvider") {
            return Ok(None);
        }
        self.handle()
            .request(
                "textDocument/documentSymbol",
                json!({ "textDocument": { "uri": uri } }),
            )
            .map(Some)
    }

    /// Shut the server down (graceful, then kill). Idempotent.
    pub fn shutdown(&mut self) {
        if let Some(handle) = self.handle.take() {
            let _ = handle.shutdown();
        }
    }
}

impl Drop for LspServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// The filesystem path behind a `file:` URI string, for iterating the
/// server's opened documents.
fn uri_file_path(uri: &str) -> Option<PathBuf> {
    url::Url::parse(uri).ok()?.to_file_path().ok()
}

/// The `file:` URI of a canonical path.
pub fn path_uri(path: &Path) -> Result<String> {
    url::Url::from_file_path(path)
        .map(|url| url.to_string())
        .map_err(|_| anyhow::anyhow!("path {} is not absolute", path.display()))
}

mod warm;

#[cfg(test)]
mod tests;
