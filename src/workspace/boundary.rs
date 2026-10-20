//! The query boundary: what the watcher delivered since the last
//! query, told to the servers that would answer this one before any
//! index is read (`docs/correctness.md`).

use super::Workspace;
use crate::discovery::evidence::skipped;
use crate::host::observation::Event;
use crate::lsp::server::FileChangeKind;
use anyhow::Result;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[cfg(test)]
mod tests;

impl Workspace {
    /// The query boundary: bring every server that would answer a query
    /// in `scope` up to the tree before the query reads any index
    /// (`docs/correctness.md`). Each server is reconciled against its own
    /// census — every served file written, created, or removed since is
    /// pushed or closed — and then told what the watcher delivered, which
    /// reaches the files no census covers: manifests, configuration, any
    /// file outside the server's languages. The servers are the ones the
    /// query will ask — `scope` narrowed to `language` — by the same
    /// routing rule (`docs/ownership.md`).
    pub(crate) fn query_boundary(
        &mut self,
        scope: Option<&Path>,
        language: Option<&str>,
    ) -> Result<()> {
        // A scope nothing serves reconciles and is told nothing — what the
        // watcher delivered is still drained — and the query itself
        // reports why nothing serves it.
        let selected = self.select(scope, language).unwrap_or_default();
        for &index in &selected {
            self.server_mut(index)?.reconcile()?;
        }
        self.flush_watched(&selected)?;
        Ok(())
    }

    /// The selected servers whose census stopped at the file budget, so
    /// the query boundary did not reach every file they serve.
    pub(crate) fn unreconciled(&self, selected: &[usize]) -> Vec<String> {
        selected
            .iter()
            .filter_map(|&index| self.server(index).ok())
            .filter(|server| server.census_truncated())
            .map(|server| server.name().to_string())
            .collect()
    }

    /// The watcher's half of the query boundary: deliver the
    /// filesystem changes that landed since the last query to the
    /// servers that would answer this one (`docs/correctness.md`).
    ///
    /// A drain reduces to one change per path — the file's state
    /// on disk now, not its event history — and is never sent for
    /// a document the server holds: the query's own sync re-feeds
    /// that file with its exact text (`docs/correctness.md`). An
    /// overflow event
    /// names no path, so what the lost window changed is unknown:
    /// the reconciliation is the whole tree, re-stated
    /// (`docs/observation.md`).
    fn flush_watched(&mut self, selected: &[usize]) -> Result<()> {
        let events = self.pending.drain();
        if events.is_empty() {
            return Ok(());
        }
        let overflowed = events.iter().any(|event| matches!(event, Event::Overflow));
        let changes = reduce_watched(&self.root, &events);
        // The reconciliation is computed once and filtered per
        // server, each of which holds a different set of owned
        // documents.
        let restated = if overflowed {
            Some(restate_tree(&self.root))
        } else {
            None
        };
        for &index in selected {
            let server = self.server_mut(index)?;
            if !server.watched_files() {
                continue;
            }
            let mut batch = changes.clone();
            if let Some(restated) = &restated {
                batch.extend(restated.iter().cloned());
            }
            batch.retain(|(path, _)| !server.owns(path));
            if batch.is_empty() {
                continue;
            }
            server.notify_watched_files(&batch)?;
        }
        Ok(())
    }
}

/// Reduce a drain of events to one change per path, in order of
/// first appearance: the last event for a path, corrected against
/// the disk — a path whose last event says it was created or
/// changed but no longer exists was deleted after the event
/// landed. What a server needs is the file's state now, not the
/// history of how it got there. A path under a directory the evidence
/// scan skips is dropped: build output and dependency trees are not
/// sources, and a server's own build writes there — told back to it,
/// those writes ask it to re-read what it just produced.
fn reduce_watched(root: &Path, events: &[Event]) -> Vec<(PathBuf, FileChangeKind)> {
    let mut order: Vec<PathBuf> = Vec::new();
    let mut last: HashMap<PathBuf, FileChangeKind> = HashMap::new();
    for event in events {
        let (path, kind) = match event {
            Event::Created { path } => (path.clone(), FileChangeKind::Created),
            Event::Changed { path } => (path.clone(), FileChangeKind::Changed),
            Event::Removed { path } => (path.clone(), FileChangeKind::Deleted),
            // An overflow event names no path; the caller
            // reconciles it.
            Event::Overflow => continue,
        };
        if under_skipped(root, &path) {
            continue;
        }
        if last.insert(path.clone(), kind).is_none() {
            order.push(path);
        }
    }
    order
        .into_iter()
        .map(|path| {
            let kind = if path.exists() {
                last[&path]
            } else {
                FileChangeKind::Deleted
            };
            (path, kind)
        })
        .collect()
}

/// Every file under `root` that the evidence scan would see, for
/// the reconciliation an overflow asks for: the lost window's
/// changes are unknown, so the tree is re-stated in full and each
/// server re-reads what it holds (`docs/observation.md`). The walk
/// mirrors the evidence scan's — the same skipped directories, the
/// same budget, unreadable entries skipped rather than failed
/// (`src/discovery/evidence.rs`) — so build output and dependency
/// trees are not re-stated.
fn restate_tree(root: &Path) -> Vec<(PathBuf, FileChangeKind)> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        if files.len() >= crate::discovery::evidence::FILE_BUDGET {
            // The budget the evidence scan itself stops at: a tree
            // past it is reconciled as far as the budget reaches.
            break;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            // `file_type` does not follow links: a symlinked
            // directory is neither branch, and is not followed, as in
            // the evidence scan.
            if file_type.is_dir() {
                let name = entry.file_name();
                let Some(name) = name.to_str() else {
                    continue;
                };
                if skipped(name) {
                    continue;
                }
                stack.push(entry.path());
            } else if file_type.is_file() {
                files.push((entry.path(), FileChangeKind::Changed));
            }
        }
    }
    files
}

/// Whether `path` lies under a directory the evidence scan skips,
/// judged on the directories between `root` and the file. A path
/// outside the root is not judged here.
fn under_skipped(root: &Path, path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(root) else {
        return false;
    };
    let Some(parent) = relative.parent() else {
        return false;
    };
    parent
        .components()
        .any(|component| component.as_os_str().to_str().is_some_and(skipped))
}
