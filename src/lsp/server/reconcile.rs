//! Reconciliation at the query boundary: the tree as it is now, against
//! the census the server was last brought up to, pushed as content
//! (`docs/correctness.md`).
//!
//! A change notice asks the server to re-read a file itself, and whether
//! its index has caught up by the time it answers is the server's. A push
//! hands it the text: a file created or written since the last census is
//! synced the way a file-scoped query syncs its file, and is owned from
//! then on, so every later query re-pushes it by content. Nothing here
//! waits on the platform's delivery of a change — the census is a walk,
//! and a walk sees a write the moment it is on disk.

use super::{FileChangeKind, LspServer, census::Census};
use anyhow::Result;

impl LspServer {
    /// Bring the server up to the tree: re-take the census, push every
    /// served file created or written since the last one, and tell it of
    /// every one removed. Called before a query reads any index.
    ///
    /// A file that cannot be read as text by the time it is pushed — gone
    /// between the walk and the read, or not UTF-8 — is left to the
    /// server's own reading of the disk: no query named it, and the query
    /// that does names the failure.
    pub fn reconcile(&mut self) -> Result<()> {
        let now = Census::take(&self.root, self.adapter.as_ref());
        let changes = self.census.changes_until(&now);
        for path in &changes.changed {
            let _ = self.sync(path);
        }
        if !changes.deleted.is_empty() {
            for path in &changes.deleted {
                // An owned document is closed first: the server would
                // otherwise keep answering from the text it was handed.
                self.moved(path)?;
            }
            if self.watched_files() {
                let deleted: Vec<_> = changes
                    .deleted
                    .iter()
                    .map(|path| (path.clone(), FileChangeKind::Deleted))
                    .collect();
                self.notify_watched_files(&deleted)?;
            }
        }
        self.census = now;
        Ok(())
    }

    /// Whether the census stopped at the evidence scan's file budget, so a
    /// change past it is not reconciled before a query.
    pub fn census_truncated(&self) -> bool {
        self.census.truncated()
    }
}
