//! The watched-file notification: what this client tells a
//! server about files it does not hold, and the state that
//! decides whether it should.
//!
//! The notification is how a query stays current for files no
//! query has opened: a bare-name query owns no file, so nothing
//! else in the pipeline says the file changed
//! (`docs/correctness.md`). It is sent at the query boundary,
//! before the query that follows on the same wire, for every
//! change the watcher delivered before the query was admitted.
//! Ordering says the server receives it first; whether its
//! index has caught up by the time it answers is the server's,
//! and the boundary `docs/correctness.md` states is what holds.

use std::path::{Path, PathBuf};

use anyhow::Result;
use serde::Serialize;
use serde_json::json;

use super::LspServer;
use super::path_uri;

/// What a `workspace/didChangeWatchedFiles` entry says happened
/// to one file: the LSP `FileChangeType` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileChangeKind {
    Created,
    Changed,
    Deleted,
}

impl FileChangeKind {
    /// The wire value: 1 created, 2 changed, 3 deleted.
    fn as_lsp(self) -> u8 {
        match self {
            Self::Created => 1,
            Self::Changed => 2,
            Self::Deleted => 3,
        }
    }
}

/// One `workspace/didChangeWatchedFiles` entry.
#[derive(Debug, Clone, Serialize)]
struct FileChange {
    uri: String,
    #[serde(rename = "type")]
    kind: u8,
}

impl LspServer {
    /// Whether `workspace/didChangeWatchedFiles` notifications
    /// should be sent to this server. It either never dynamically
    /// registered — the protocol has no static form, so it is sent
    /// them unsolicited — or holds registrations now. A server that
    /// registered and then withdrew every registration has asked not
    /// to be told.
    pub fn watched_files(&self) -> bool {
        self.handle().watched_files()
    }

    /// Whether this server holds `path` as an opened document. Its
    /// own `didChange` carries that file's exact text, so a
    /// watched-file change for it would be a redundant re-read of
    /// the same disk state.
    pub fn owns(&self, path: &Path) -> bool {
        match path_uri(path) {
            Ok(uri) => self.opened.contains_key(&uri),
            // A path that cannot be a URI cannot be a document
            // either.
            Err(_) => false,
        }
    }

    /// Tell the server what happened to files it does not hold:
    /// `workspace/didChangeWatchedFiles`, one entry per path,
    /// reduced to its current state. Sent before the query that
    /// would read the index, so the server applies it first
    /// (`docs/correctness.md`).
    pub fn notify_watched_files(&self, changes: &[(PathBuf, FileChangeKind)]) -> Result<()> {
        let changes: Vec<FileChange> = changes
            .iter()
            .map(|(path, kind)| {
                Ok(FileChange {
                    uri: path_uri(path)?,
                    kind: kind.as_lsp(),
                })
            })
            .collect::<Result<_>>()?;
        self.handle().notify(
            "workspace/didChangeWatchedFiles",
            json!({ "changes": changes }),
        )
    }
}
