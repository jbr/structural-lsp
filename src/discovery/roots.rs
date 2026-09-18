//! Where a project begins, read from the tree above a working directory.
//!
//! A pure function over the filesystem: it reads directory entries and
//! returns a value. Nothing is started, nothing is cached, and the result
//! carries the reasons it reached its answer
//! (`backlog/discovery/selection.md`).
//!
//! The rule, and why each part of it:
//!
//! - **Walk up, not down.** The common case is an agent started in
//!   `repo/backend/src` when the manifest is at `repo/backend`.
//! - **Topmost marker, not nearest.** The nearest `Cargo.toml` above a
//!   workspace member is the member's, which gives every member its own
//!   answer; the outermost is the workspace. rust-analyzer and gopls both
//!   pick the outermost for themselves when started inside a member
//!   (`reference/server-root-discovery.md`), and this is Helix's
//!   convention rather than nvim-lspconfig's.
//! - **The VCS root bounds the search and is not the answer.** A repository
//!   holding two unrelated projects and no manifest of its own has two
//!   project roots, not one; answering with the repository would fuse them.
//! - **The working directory is the floor.** A directory of shell scripts
//!   with no marker and no repository above it is its own root.
//!
//! What this decides is the *key* a workspace is shared under. It does not
//! decide what a server is told at startup, and it does not decide what an
//! answer is filtered against — those are the caller's working directory
//! and stay that way (`docs/resolution.md`). A key that comes out wrong
//! costs sharing, not correctness.

use std::path::{Path, PathBuf};

/// Ancestors examined before the walk gives up.
///
/// Only reached when no VCS root bounds the search, which is the case a
/// bound is for: a marker at `/` is far likelier to be someone's stray file
/// than a project anyone meant.
const ANCESTOR_LIMIT: usize = 64;

/// Directories whose presence marks a version-control root. The **nearest**
/// one wins, so a submodule or a nested checkout is its own span rather
/// than its parent's.
const VCS_DIRECTORIES: &[(&str, &str)] = &[
    (".git", "git"),
    (".hg", "mercurial"),
    (".svn", "subversion"),
    (".jj", "jujutsu"),
];

/// Files whose presence marks a project, and the languages each implies.
///
/// Provisional and deliberately modest: this is the shape a catalogue entry
/// will carry as data with provenance, not a table meant to grow here
/// (`backlog/discovery/selection.md`). A marker earns a row only when it
/// names a project boundary — `tsconfig.json` does, a linter's dotfile does
/// not.
const PROJECT_MARKERS: &[(&str, &[&str])] = &[
    ("Cargo.toml", &["rust"]),
    ("go.mod", &["go"]),
    ("deno.json", &["typescript", "javascript"]),
    ("deno.jsonc", &["typescript", "javascript"]),
    ("package.json", &["javascript", "typescript"]),
    ("tsconfig.json", &["typescript"]),
    ("pyproject.toml", &["python"]),
    ("setup.py", &["python"]),
    ("CMakeLists.txt", &["c", "cpp"]),
    ("compile_commands.json", &["c", "cpp"]),
    ("pom.xml", &["java"]),
    ("build.gradle", &["java", "kotlin"]),
];

/// A version-control root, and which system said so.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Vcs {
    pub path: PathBuf,
    pub kind: &'static str,
}

/// A project marker found above (or at) the working directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Marker {
    /// The directory holding the marker file — the project root it implies.
    pub path: PathBuf,
    pub file: &'static str,
    pub languages: &'static [&'static str],
}

/// Why a root is the root. Carried so a plan built on it can say where it
/// came from, which every answer in this project owes
/// (`backlog/discovery/README.md`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RootReason {
    /// The outermost project marker within the span.
    Marker {
        file: &'static str,
        /// The VCS root that bounded the search, when one did.
        bounded_by: Option<PathBuf>,
    },
    /// No marker above the working directory: it is its own root.
    WorkingDirectory {
        /// Present when a repository was found but held no marker above the
        /// working directory — the case where answering with the repository
        /// would fuse unrelated projects.
        vcs_root: Option<PathBuf>,
    },
}

/// The root a workspace is keyed by, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Root {
    pub path: PathBuf,
    pub reason: RootReason,
}

impl Root {
    /// One line, for a plan's reasons and for an operator log.
    pub fn explain(&self) -> String {
        match &self.reason {
            RootReason::Marker { file, bounded_by } => match bounded_by {
                Some(vcs) => format!(
                    "outermost `{file}` at or above the working directory, within {}",
                    vcs.display()
                ),
                None => format!(
                    "outermost `{file}` at or above the working directory (no repository found)"
                ),
            },
            RootReason::WorkingDirectory {
                vcs_root: Some(vcs),
            } => format!(
                "no project marker above the working directory within {}; the directory is its own root",
                vcs.display()
            ),
            RootReason::WorkingDirectory { vcs_root: None } => {
                "no project marker and no repository above the working directory".to_string()
            }
        }
    }
}

/// What the tree above `cwd` says about where its project begins.
///
/// Markers are **outermost first**, and within one directory in the order
/// [`PROJECT_MARKERS`] lists them, so the same tree always yields the same
/// value — a plan whose order varies between runs cannot be reproduced from
/// a report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Ancestry {
    /// The directory the caller named, as given.
    pub cwd: PathBuf,
    /// The nearest version-control root at or above `cwd`.
    pub vcs: Option<Vcs>,
    /// Markers at or above `cwd`, bounded by `vcs` when there is one.
    pub markers: Vec<Marker>,
}

impl Ancestry {
    /// Read the tree above `cwd`. Directories that cannot be read are
    /// skipped rather than failing the walk: an unreadable ancestor is a
    /// permissions fact about someone else's directory, and the answer
    /// below it is still usable.
    pub fn gather(cwd: &Path) -> Self {
        let mut vcs = None;
        // One group per directory, nearest first; the *groups* are reversed
        // once the span is known, so a directory's own markers keep the
        // order PROJECT_MARKERS lists them in. Reversing the flat list
        // instead would invert that inner order.
        let mut by_directory: Vec<Vec<Marker>> = Vec::new();
        for directory in cwd.ancestors().take(ANCESTOR_LIMIT) {
            if vcs.is_none()
                && let Some((_, kind)) = VCS_DIRECTORIES
                    .iter()
                    .find(|(name, _)| directory.join(name).is_dir())
            {
                vcs = Some(Vcs {
                    path: directory.to_path_buf(),
                    kind,
                });
            }
            let found: Vec<Marker> = PROJECT_MARKERS
                .iter()
                .filter(|(file, _)| directory.join(file).is_file())
                .map(|(file, languages)| Marker {
                    path: directory.to_path_buf(),
                    file,
                    languages,
                })
                .collect();
            if !found.is_empty() {
                by_directory.push(found);
            }
            // The VCS root bounds the search: found here, this directory is
            // the last one examined.
            if vcs.as_ref().is_some_and(|found| found.path == directory) {
                break;
            }
        }
        by_directory.reverse();
        let markers: Vec<Marker> = by_directory.into_iter().flatten().collect();
        Self {
            cwd: cwd.to_path_buf(),
            vcs,
            markers,
        }
    }

    /// The outermost marker's directory, or the working directory when
    /// there is none.
    pub fn root(&self) -> Root {
        let vcs_root = self.vcs.as_ref().map(|vcs| vcs.path.clone());
        match self.markers.first() {
            Some(marker) => Root {
                path: marker.path.clone(),
                reason: RootReason::Marker {
                    file: marker.file,
                    bounded_by: vcs_root,
                },
            },
            None => Root {
                path: self.cwd.clone(),
                reason: RootReason::WorkingDirectory { vcs_root },
            },
        }
    }

    /// Every language the markers above `cwd` imply, outermost first and
    /// deduplicated. Evidence, not proof: a `package.json` says a project
    /// boundary is here, not that any particular server applies
    /// (`backlog/discovery/selection.md`).
    pub fn languages(&self) -> Vec<&'static str> {
        let mut languages: Vec<&'static str> = Vec::new();
        for marker in &self.markers {
            for language in marker.languages {
                if !languages.contains(language) {
                    languages.push(language);
                }
            }
        }
        languages
    }
}

#[cfg(test)]
mod tests;
