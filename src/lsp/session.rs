//! The rust-analyzer session used by the MCP `show` slice.
//!
//! One server for one fixed workspace root, started eagerly at MCP-server
//! start (`Session::start` from [`crate::state::State::new`]). Readiness is
//! rust-analyzer's `experimental/serverStatus` quiescence, waited for bounded
//! on first use — a call that arrives before quiescence waits, then answers
//! with status rather than a guess. Opened documents are refreshed from disk
//! before every query so a changed file is never served from a stale
//! snapshot.
//!
//! Concurrency: the session is driven only from mcplease's single dispatch
//! thread; requests are issued strictly sequentially (`Client::request`).
//! Dispatch is serialized by construction, not by a lock.

use anyhow::{Context, Result, bail};
use lsp_types::{Position, SymbolInformation};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::lsp::client::Client;

/// How long the event log is kept for a long-running server. The log is
/// diagnostics only; readiness lives in
/// [`Client::latest_server_status`] and survives these drains. The reader
/// channel itself is not bounded by this.
const EVENT_CAP: usize = 2_000;

/// A document the session has opened, with the snapshot the server was last
/// given.
struct OpenedDoc {
    version: i32,
    text: String,
}

/// Readiness, as evidence rather than a completeness certificate.
#[derive(Debug, Clone, Copy)]
pub enum Readiness {
    /// No quiescent `serverStatus` observed yet. Queries may proceed after a
    /// bounded wait expires, but the tool says results may be incomplete.
    Pending,
    /// A `serverStatus` with `quiescent: true` was observed this long after
    /// server start — measured from the start of the current quiescent epoch
    /// (a later `quiescent: false` ends the epoch; the next `true` begins a
    /// new one). Evidence of what was last observed, never a completeness
    /// claim.
    Quiescent { after: Duration },
}

impl Readiness {
    /// Fold one observed status into the epoch. Pure so the transitions are
    /// unit-testable without a server: `true` starts an epoch when none is
    /// running, `false` ends one, absence changes nothing.
    pub fn update(self, status: Option<&serde_json::Value>, elapsed: Duration) -> Self {
        let quiescent = status.and_then(|value| value.get("quiescent"));
        match quiescent {
            Some(serde_json::json!(true)) => match self {
                Readiness::Pending => Readiness::Quiescent { after: elapsed },
                epoch @ Readiness::Quiescent { .. } => epoch,
            },
            Some(_) => Readiness::Pending,
            None => self,
        }
    }
}

pub struct Session {
    root: PathBuf,
    client: Client,
    readiness: Readiness,
    started_at: Instant,
    /// `serverInfo` from `initialize`, when the server reports one.
    server_info: Option<Value>,
    /// The position encoding the server negotiated at `initialize` (LSP
    /// default is UTF-16 when absent).
    position_encoding: Option<String>,
    /// Documents opened with the server, keyed by URI, with the snapshot the
    /// server was last given.
    opened: HashMap<String, OpenedDoc>,
    /// Bound used and observed for the latest readiness wait — evidence for
    /// result footers.
    pub last_wait: Option<Duration>,
}

impl Session {
    /// Spawn rust-analyzer and initialize it against `root`. Cheap (initialize
    /// measured in the tens of ms); readiness waits happen on query time.
    pub fn start(root: &Path) -> Result<Self> {
        let mut client = Client::spawn("rust-analyzer", &[])?;
        client.set_event_cap(Some(EVENT_CAP));
        let started_at = Instant::now();

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
                        // Full-text changes: `refresh_document` sends the
                        // whole file when the disk copy differs.
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
                // rust-analyzer sends `experimental/serverStatus` only when
                // the client advertises this — our readiness signal.
                "experimental": { "serverStatusNotification": true }
            }
        });

        let result = client.request("initialize", params, Duration::from_secs(60))?;
        let server_info = result.get("serverInfo").cloned();
        let position_encoding = result
            .pointer("/capabilities/positionEncoding")
            .and_then(Value::as_str)
            .map(String::from);
        client.notify("initialized", json!({}))?;

        Ok(Self {
            root: root.to_path_buf(),
            client,
            readiness: Readiness::Pending,
            started_at,
            server_info,
            position_encoding,
            opened: HashMap::new(),
            last_wait: None,
        })
    }

    #[allow(dead_code)]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[allow(dead_code)]
    pub fn readiness(&self) -> &Readiness {
        &self.readiness
    }

    pub fn server_info(&self) -> Option<&Value> {
        self.server_info.as_ref()
    }

    pub fn position_encoding(&self) -> Option<&str> {
        self.position_encoding.as_deref()
    }

    /// The latest `experimental/serverStatus` notification, as evidence for
    /// result footers. Protocol state on the client, not the event log.
    pub fn latest_server_status(&self) -> Option<&Value> {
        self.client.latest_server_status.as_ref()
    }

    /// Fold the latest observed `serverStatus` into the readiness epoch. A
    /// `quiescent: false` ends any quiescent epoch (re-indexing may have
    /// started); the next `true` begins a new one. No status leaves the
    /// epoch unchanged.
    fn update_readiness(&mut self) {
        self.readiness = self.readiness.update(
            self.client.latest_server_status.as_ref(),
            self.started_at.elapsed(),
        );
    }

    /// The readiness as of the latest observed status, updated in place —
    /// for rendering current evidence in results, not a completeness claim.
    pub fn readiness_current(&mut self) -> Readiness {
        self.update_readiness();
        self.readiness
    }

    /// Let a just-fed change reach the server (a short pump so a
    /// `quiescent: false` has the chance to arrive first), then wait, bounded,
    /// for the epoch to go quiescent again. Used between feeding a changed
    /// file and re-querying against it. Returns the readiness: a caller that
    /// receives [`Readiness::Pending`] must answer with status and stop
    /// issuing semantic queries against the unsettled index.
    pub fn settle(&mut self, bound: Duration) -> Result<Readiness> {
        self.client.pump(Duration::from_millis(250))?;
        self.ensure_ready(bound)
    }

    /// Wait, bounded, for observed quiescence. Returns the (possibly still
    /// pending) readiness; a pending return is the caller's cue to answer
    /// with status rather than results. Readiness follows the latest status
    /// (a `false` seen after a `true` returns it to pending), so a repeat
    /// call after re-indexing waits again. Server death surfaces as `Err`.
    pub fn ensure_ready(&mut self, bound: Duration) -> Result<Readiness> {
        // Decide from what has actually arrived: drain every message already
        // queued (a status sent while the caller was idle must be seen) —
        // checking the cached status first would present a stale quiescent
        // epoch as current.
        self.client.drain_queued()?;
        let deadline = Instant::now() + bound;
        loop {
            self.update_readiness();
            if let Readiness::Quiescent { .. } = self.readiness {
                break;
            }
            let now = Instant::now();
            if now >= deadline {
                self.last_wait = Some(bound);
                return Ok(self.readiness); // still pending
            }
            self.client.pump(
                deadline
                    .saturating_duration_since(now)
                    .min(Duration::from_millis(250)),
            )?;
        }
        self.last_wait = Some(self.started_at.elapsed());
        Ok(self.readiness)
    }

    /// Re-read every document the session has previously opened and feed the
    /// server when its copy is stale. Called before discovery so
    /// `workspace/symbol` and position math run against the server's current
    /// view of those files. Returns how many documents were (re)fed.
    pub fn refresh_opened(&mut self) -> Result<usize> {
        let paths: Vec<PathBuf> = self
            .opened
            .keys()
            .filter_map(|uri| uri_file_path(uri))
            .collect();
        let mut fed = 0usize;
        for path in paths {
            fed += self
                .refresh_document(&path)
                .map(|(_, was_fed, _)| usize::from(was_fed))
                .map_err(|error| {
                    error.context("refreshing a previously opened document before the query")
                })?;
        }
        Ok(fed)
    }

    /// Ensure the server's copy of `path` matches disk: open it on first use,
    /// send a full-text `didChange` when the file changed since the last
    /// snapshot, do nothing when unchanged. Returns the URI, whether the
    /// server was (re)fed, and the on-disk text just read — the same bytes
    /// the server now holds, so callers convert positions against them.
    ///
    /// A previously opened file that is now missing or unreadable is an
    /// actionable error and its stale snapshot is discarded, so no later call
    /// keeps querying the old in-memory document silently. A never-opened
    /// unreadable file is a plain actionable error.
    pub fn refresh_document(&mut self, path: &Path) -> Result<(String, bool, String)> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) => {
                // A previously opened file that has become unreadable:
                // discard its snapshot AND tell the server the document is no
                // longer open on our side. Without the didClose the server
                // would keep its unsaved snapshot (a ghost symbol a later
                // query could still resolve) and a recreated file would hit
                // its duplicate-didOpen guard. The normal path must not touch
                // `opened` — the lookup below decides between didOpen and
                // didChange.
                let was_opened = match path_uri(path) {
                    Ok(uri) => {
                        if self.opened.remove(&uri).is_some() {
                            let _ = self.client.notify(
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
                    format!("reading {} to refresh the server's copy", path.display())
                };
                return Err(error).context(context);
            }
        };
        let uri = path_uri(path)?;
        match self.opened.get(&uri) {
            Some(doc) if doc.text == text => Ok((uri, false, text)),
            Some(doc) => {
                let version = doc.version + 1;
                self.client.notify(
                    "textDocument/didChange",
                    json!({
                        "textDocument": { "uri": uri, "version": version },
                        "contentChanges": [ { "text": text } ],
                    }),
                )?;
                self.opened.insert(
                    uri.clone(),
                    OpenedDoc {
                        version,
                        text: text.clone(),
                    },
                );
                Ok((uri, true, text))
            }
            None => {
                self.client.notify(
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
                Ok((uri, true, text))
            }
        }
    }

    /// `workspace/symbol` query, decoded into candidates as the server
    /// reported them (no completeness claim is made anywhere downstream).
    pub fn workspace_symbol(
        &mut self,
        query: &str,
        timeout: Duration,
    ) -> Result<Vec<SymbolInformation>> {
        self.check_encoding()?;
        let result = self
            .client
            .request("workspace/symbol", json!({ "query": query }), timeout)?;
        let infos: Option<Vec<SymbolInformation>> =
            serde_json::from_value(result).context("parsing workspace/symbol result")?;
        Ok(infos.unwrap_or_default())
    }

    /// `textDocument/definition` at a position; the raw result is normalized
    /// by [`crate::lsp::definition::parse_definition_result`].
    pub fn definition(
        &mut self,
        uri: &str,
        position: Position,
        timeout: Duration,
    ) -> Result<Value> {
        self.check_encoding()?;
        self.client.request(
            "textDocument/definition",
            json!({
                "textDocument": { "uri": uri },
                "position": position,
            }),
            timeout,
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

    /// Ask the server to shut down and exit; the client kill/reaps regardless.
    #[allow(dead_code)]
    pub fn shutdown(&mut self) {
        let _ = self.client.shutdown();
    }
}

impl Drop for Session {
    /// On process exit, attempt the polite shutdown handshake (bounded), then
    /// rely on the client's kill/reap cleanup regardless.
    fn drop(&mut self) {
        let _ = self.client.shutdown();
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
mod tests {
    use super::*;

    #[test]
    fn path_uri_requires_an_absolute_path() {
        assert!(path_uri(Path::new("/tmp/x/lib.rs")).is_ok());
        assert!(path_uri(Path::new("relative/lib.rs")).is_err());
    }

    // ---- lifecycle regressions (real rust-analyzer; run with -- --ignored) ----

    /// A scratch copy of the fixture crate, so edits and deletions are safe.
    fn scratch_fixture(tag: &str) -> std::path::PathBuf {
        let base =
            std::env::temp_dir().join(format!("structural-session-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("src")).unwrap();
        let manifest = concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/rust");
        std::fs::copy(format!("{manifest}/Cargo.toml"), base.join("Cargo.toml")).unwrap();
        std::fs::copy(format!("{manifest}/src/lib.rs"), base.join("src/lib.rs")).unwrap();
        base
    }

    fn events_with_method(session: &Session, method: &str) -> Vec<usize> {
        session
            .client
            .events
            .iter()
            .enumerate()
            .filter(|(_, event)| event.get("method").and_then(Value::as_str) == Some(method))
            .map(|(index, _)| index)
            .collect()
    }

    /// (B) The settle contract: a bound that expires without observed
    /// quiescence returns Pending — the caller's cue to answer with status
    /// and issue no semantic query — and a full-bound settle recovers.
    ///
    /// Driven from a cold start, where no `serverStatus` has arrived yet:
    /// deterministic, unlike forcing a post-edit `quiescent=false` (r-a's
    /// emission timing there is not stable enough to assert against). The
    /// tool layer's Pending guards sit at every settle call site.
    #[test]
    #[ignore = "spawns rust-analyzer; run explicitly with -- --ignored"]
    fn settle_reports_pending_within_a_tiny_bound_and_recovers() {
        let root = scratch_fixture("settle-pending");
        let mut session = Session::start(&root).unwrap();

        // Cold start: no serverStatus yet. A zero-width bound cannot observe
        // quiescence, so settle must report Pending, not block or lie.
        let readiness = session.settle(Duration::ZERO).expect("settle");
        assert!(
            matches!(readiness, Readiness::Pending),
            "no status has arrived; a zero bound expires as Pending: {readiness:?}"
        );

        // The full bound waits out the cold start and reports the epoch.
        let readiness = session
            .settle(crate::state::QUIESCE_BOUND)
            .expect("settle with the full bound");
        assert!(
            matches!(readiness, Readiness::Quiescent { .. }),
            "the full bound reaches quiescence: {readiness:?}"
        );

        session.shutdown();
        let _ = std::fs::remove_dir_all(&root);
    }

    /// (C) Deleting a previously opened document discards the snapshot AND
    /// closes it on the server (didClose after the didOpen); recreating the
    /// file produces a fresh didOpen — a legal sequence, rather than a
    /// duplicate didOpen against a server that never learned the file closed.
    #[test]
    #[ignore = "spawns rust-analyzer; run explicitly with -- --ignored"]
    fn deleting_an_opened_document_closes_it_on_the_server_and_recovers() {
        let root = scratch_fixture("delete-close");
        let mut session = Session::start(&root).unwrap();
        session
            .ensure_ready(crate::state::QUIESCE_BOUND)
            .expect("cold start reaches quiescence");

        let lib_path = root.join("src/lib.rs");
        let original = std::fs::read_to_string(&lib_path).unwrap();
        let uri = path_uri(&lib_path).unwrap();

        let (_uri, fed, _text) = session.refresh_document(&lib_path).unwrap();
        assert!(fed);
        assert!(session.opened.contains_key(&uri));
        let opens_before = events_with_method(&session, "textDocument/didOpen");
        assert_eq!(opens_before.len(), 1);

        // Delete: actionable error, snapshot discarded, didClose sent.
        std::fs::remove_file(&lib_path).unwrap();
        let error = session
            .refresh_document(&lib_path)
            .expect_err("a deleted opened file is an actionable error");
        assert!(
            error
                .to_string()
                .contains("opened earlier but is now missing"),
            "{error}"
        );
        assert!(
            !session.opened.contains_key(&uri),
            "the stale snapshot is discarded"
        );
        let closes = events_with_method(&session, "textDocument/didClose");
        assert_eq!(closes.len(), 1, "exactly one didClose for the discard");
        assert!(
            closes[0] > opens_before[0],
            "the didClose follows the didOpen"
        );

        // Recreate: a fresh didOpen — legal after didClose; the server never
        // saw a duplicate didOpen for a still-open document.
        std::fs::write(&lib_path, &original).unwrap();
        let (_uri, fed, text) = session.refresh_document(&lib_path).unwrap();
        assert!(fed);
        assert_eq!(text, original);
        assert!(session.opened.contains_key(&uri));
        let opens_after = events_with_method(&session, "textDocument/didOpen");
        assert_eq!(opens_after.len(), 2, "one open per server-side open state");
        assert!(
            opens_after[1] > closes[0],
            "the recreated didOpen comes after the didClose"
        );

        session.shutdown();
        let _ = std::fs::remove_dir_all(&root);
    }
}
