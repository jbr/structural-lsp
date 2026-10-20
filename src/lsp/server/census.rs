//! What a server could have read from the tree: one stat per file it
//! serves, taken before its process existed and re-taken at every query
//! boundary (`docs/correctness.md`).
//!
//! The census is how a query stays current without waiting on the
//! platform's change delivery. Two censuses differ wherever a served file
//! was created, written, or removed between them, and the difference is
//! pushed as content — the push an owned file already gets — so the
//! answer that follows reads text the server was just handed, not a
//! notice it may not have acted on yet.
//!
//! A stamp is modification time and length. A write that changes neither —
//! same length, within one tick of the filesystem's timestamp — is the one
//! a stamp cannot see, so a file whose stamp lies within [`RACY`] of the
//! census that recorded it is treated as changed by the next one: it is
//! pushed once, and owned from then on, where its content rather than its
//! stamp is compared (`git`'s answer to the same problem).

use crate::discovery::evidence::{FILE_BUDGET, skipped};
use crate::lsp::adapter::{Adapter, language_for};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// How close to its census a file's modification time can be before its
/// stamp is not trusted to show a later write. Two seconds covers the
/// coarsest timestamp a mounted filesystem is likely to keep (FAT's); a
/// filesystem with nanosecond times loses nothing to it but one push of a
/// file written in the two seconds before a census.
const RACY: Duration = Duration::from_secs(2);

/// One served file's stamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp {
    modified: Option<SystemTime>,
    len: u64,
}

/// The served files under a root, stamped, as of `taken_at`.
#[derive(Debug)]
pub(crate) struct Census {
    taken_at: SystemTime,
    stamps: HashMap<PathBuf, Stamp>,
    /// The walk stopped at the evidence scan's budget, so files past it
    /// are not in the census and their changes are not seen.
    truncated: bool,
}

/// What differs between two censuses.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Changes {
    /// Created, written, or racy: each to be pushed as content.
    pub(crate) changed: Vec<PathBuf>,
    /// In the earlier census and gone from the later one.
    pub(crate) deleted: Vec<PathBuf>,
}

impl Census {
    /// Stamp every file under `root` the adapter serves, walking as the
    /// evidence scan does: the same skipped directories, symlinked
    /// directories not followed, unreadable entries skipped, and the same
    /// file budget (`docs/discovery.md`).
    pub(crate) fn take(root: &Path, adapter: &dyn Adapter) -> Self {
        let languages = adapter.languages();
        // Read before the walk: a write during the walk then lands after
        // `taken_at`, inside the racy window or past it.
        let taken_at = SystemTime::now();
        let mut stamps = HashMap::new();
        let mut truncated = false;
        let mut files_seen = 0usize;
        let mut pending = vec![root.to_path_buf()];
        'walk: while let Some(directory) = pending.pop() {
            let Ok(entries) = std::fs::read_dir(&directory) else {
                continue;
            };
            for entry in entries.flatten() {
                let Ok(kind) = entry.file_type() else {
                    continue;
                };
                // `file_type` does not follow links: a symlinked directory
                // is neither branch, and is not followed.
                if kind.is_dir() {
                    if !skipped(&entry.file_name().to_string_lossy()) {
                        pending.push(entry.path());
                    }
                } else if kind.is_file() {
                    if files_seen >= FILE_BUDGET {
                        truncated = true;
                        break 'walk;
                    }
                    files_seen += 1;
                    let path = entry.path();
                    if language_for(&languages, &path).is_none() {
                        continue;
                    }
                    let Ok(metadata) = entry.metadata() else {
                        continue;
                    };
                    stamps.insert(
                        path,
                        Stamp {
                            modified: metadata.modified().ok(),
                            len: metadata.len(),
                        },
                    );
                }
            }
        }
        Self {
            taken_at,
            stamps,
            truncated,
        }
    }

    /// What `later` holds that this census does not: files new to it,
    /// files whose stamp moved, files whose stamp this census could not
    /// trust, and files gone. Each list is sorted, so what is pushed is
    /// pushed in a stable order.
    pub(crate) fn changes_until(&self, later: &Census) -> Changes {
        let racy_after = self.taken_at.checked_sub(RACY);
        let mut changes = Changes::default();
        for (path, stamp) in &later.stamps {
            let changed = match self.stamps.get(path) {
                None => true,
                Some(before) if before != stamp => true,
                Some(before) => match (before.modified, racy_after) {
                    (Some(modified), Some(racy_after)) => modified >= racy_after,
                    // No modification time to compare, or a clock too
                    // early to subtract from: nothing says the file is
                    // unchanged.
                    _ => true,
                },
            };
            if changed {
                changes.changed.push(path.clone());
            }
        }
        for path in self.stamps.keys() {
            if !later.stamps.contains_key(path) {
                changes.deleted.push(path.clone());
            }
        }
        changes.changed.sort();
        changes.deleted.sort();
        changes
    }

    pub(crate) fn truncated(&self) -> bool {
        self.truncated
    }
}

#[cfg(test)]
mod tests;
