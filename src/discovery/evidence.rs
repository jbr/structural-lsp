//! What a tree says about itself: the markers above it and the sources
//! inside it.
//!
//! The first layer of `backlog/discovery/selection.md`. Like
//! [`super::roots`], a pure function over the filesystem returning a value
//! with its reasons — nothing is spawned, nothing is cached, and no lock is
//! taken, so every case is a test on a constructed tree.
//!
//! **Evidence, not proof.** A `package.json` says a project boundary is
//! here; it does not say that any particular server applies, or that its
//! command exists. Evidence yields candidates and reasons, and
//! [`super::selection`] decides what to do about them.
//!
//! Two deliberate cuts, so they are stated rather than discovered:
//!
//! - **Counts, not a per-directory index.** The plan names "extensions
//!   present, by count and by directory". Counts are what selection needs
//!   to decide applicability and to explain it; the per-directory form is
//!   for scoping a plan below the root, which nothing does yet. It lands
//!   with the unit that reads it rather than sitting here unused.
//! - **A budget, not a full walk.** A tree large enough to exhaust the
//!   budget has already said what language it is a thousand files ago. The
//!   result records that it stopped, because evidence that silently saw
//!   part of a tree would let a language present only in the unscanned
//!   part vanish without trace.

use super::roots::Ancestry;
use std::collections::BTreeMap;
use std::path::Path;

/// Files examined before the scan stops and says so.
///
/// Sized so that the extension mix of any ordinary project is settled long
/// before it is reached, and so that a scan of something pathological ends.
pub const FILE_BUDGET: usize = 20_000;

/// Directories never descended into: build output, dependency trees, and
/// version-control internals. Their contents describe a toolchain's
/// bookkeeping rather than what the project is written in — `node_modules`
/// alone would make almost any tree look like a JavaScript project.
pub const SKIPPED_DIRECTORIES: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    ".jj",
    "node_modules",
    "target",
    "build",
    "dist",
    "vendor",
    "venv",
    ".venv",
    ".tox",
    "__pycache__",
    ".mypy_cache",
    ".pytest_cache",
    "zig-cache",
    "zig-out",
    ".stack-work",
    "_build",
    "Pods",
    ".gradle",
];

/// The extensions a tree contains, with how many files carry each.
///
/// Counts are ordered by extension rather than by frequency: a
/// [`BTreeMap`] so that the same tree always yields the same value, since
/// directory iteration order is not guaranteed and a plan whose order
/// varies between runs cannot be reproduced from a report.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Sources {
    /// Lowercased extension (no dot) to the number of files carrying it.
    pub by_extension: BTreeMap<String, usize>,
    /// Files examined, including those with no extension.
    pub files_seen: usize,
    /// The scan stopped at [`FILE_BUDGET`] rather than reaching the end of
    /// the tree, so an absent extension is not evidence of absence.
    pub truncated: bool,
}

impl Sources {
    /// Walk `root` downwards, skipping [`SKIPPED_DIRECTORIES`] and hidden
    /// directories.
    ///
    /// Directories that cannot be read are skipped rather than failing the
    /// scan, for the same reason the upward walk skips them: an unreadable
    /// directory is a permissions fact about someone else's files, and the
    /// evidence from the rest of the tree is still usable.
    pub fn scan(root: &Path) -> Self {
        let mut sources = Sources::default();
        // Explicit stack rather than recursion: a deep or symlinked tree
        // should exhaust a budget, not the call stack.
        let mut pending = vec![root.to_path_buf()];
        while let Some(directory) = pending.pop() {
            let Ok(entries) = std::fs::read_dir(&directory) else {
                continue;
            };
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                let Ok(kind) = entry.file_type() else { continue };
                if kind.is_dir() {
                    // Symlinked directories are not followed: a link into
                    // a parent makes the walk unbounded, and a link out of
                    // the tree counts files that are not this project's.
                    if kind.is_symlink() || skipped(&name) {
                        continue;
                    }
                    pending.push(entry.path());
                } else if kind.is_file() {
                    if sources.files_seen >= FILE_BUDGET {
                        sources.truncated = true;
                        return sources;
                    }
                    sources.files_seen += 1;
                    if let Some(extension) = extension_of(&name) {
                        *sources.by_extension.entry(extension).or_default() += 1;
                    }
                }
            }
        }
        sources
    }

    /// How many files carry `extension`.
    pub fn count(&self, extension: &str) -> usize {
        self.by_extension.get(extension).copied().unwrap_or(0)
    }
}

/// A directory the scan does not descend into. Hidden directories are
/// skipped wholesale: `.git` and friends are named above, and the rest are
/// tool state rather than a project's sources.
fn skipped(name: &str) -> bool {
    name.starts_with('.') || SKIPPED_DIRECTORIES.contains(&name)
}

/// The lowercased extension of a file name, or `None` for a name with no
/// extension or a leading-dot name like `.gitignore` — which is a
/// configuration file called `.gitignore`, not a file with extension
/// `gitignore`.
fn extension_of(name: &str) -> Option<String> {
    let (stem, extension) = name.rsplit_once('.')?;
    if stem.is_empty() || extension.is_empty() {
        return None;
    }
    Some(extension.to_ascii_lowercase())
}

/// Everything read from the tree, in one value: the markers above the
/// working directory and the sources within the root they imply.
///
/// The scan starts at the **root**, not at the working directory, because
/// the root is what a server will be told and what the workspace is keyed
/// by; sources found only in a sibling member are still this project's
/// (`backlog/discovery/selection.md`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Evidence {
    pub ancestry: Ancestry,
    pub sources: Sources,
}

impl Evidence {
    /// Read the tree above and below `cwd`.
    pub fn gather(cwd: &Path) -> Self {
        let ancestry = Ancestry::gather(cwd);
        let sources = Sources::scan(&ancestry.root().path);
        Self { ancestry, sources }
    }

    /// Build from parts, for tests that supply sources directly.
    pub fn from_parts(ancestry: Ancestry, sources: Sources) -> Self {
        Self { ancestry, sources }
    }
}

#[cfg(test)]
mod tests;
