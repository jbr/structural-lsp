//! Where a candidate is said to be, and how closely a qualifier names it.
//!
//! A container has more than one source and servers disagree about which
//! they use (`docs/resolution.md`): the `containerName` field, the server's
//! own spelling of the symbol's name, the items the file's outline encloses
//! it in, and the file itself. Any one confirming the qualifier is enough —
//! they are alternative places to say the same thing, not claims that have
//! to agree — and a candidate none confirms is listed rather than silently
//! dropped.

use super::{Qualified, advertised_container, container_matches, segments};
use crate::lsp::definition::Candidate;
use std::path::Path;

/// How closely a qualifier names where a candidate is, closest first. Only
/// the closest present is kept (`partition`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Closeness {
    /// The qualifier ends the candidate's place: its file path followed by
    /// its container, or its file path when nothing in the file encloses it.
    /// `config::Config::new`, `Config::new`, `tools::listing::render`,
    /// `helper.choose` in `src/helper.gleam`.
    Place,
    /// The qualifier ends the file's path, and the candidate is inside
    /// something in that file: `listing::new` for a method in `listing.rs`.
    InFile,
    /// The qualifier ends the directory the file is in, followed by the
    /// candidate's container, or alone when nothing encloses it: the
    /// directory is the module for `mod.rs`, `__init__.py` and a Go package,
    /// and the place a parent module re-exports from (`observation::reduce`
    /// in `observation/mod.rs`, `pkg.helper` in `pkg/__init__.py`).
    DirectoryPlace,
    /// The qualifier ends the directory, and the candidate is inside
    /// something in its file.
    InDirectory,
}

/// One exact candidate and every place it is said to be.
#[derive(Debug)]
pub struct Placed {
    pub candidate: Candidate,
    /// The candidate's file under the view: its directories, then its stem.
    /// Only the stem for a file outside the view, and empty for a candidate
    /// with no file path.
    path: Vec<String>,
    /// What the file's outline encloses the candidate in, when it was asked.
    nesting: Option<String>,
}

impl Placed {
    pub fn new(root: &Path, candidate: Candidate) -> Self {
        let path = candidate
            .canonical_path
            .as_deref()
            .map(|file| file_path(root, file))
            .unwrap_or_default();
        Placed {
            candidate,
            path,
            nesting: None,
        }
    }

    /// Record what the outline encloses the candidate in. A candidate the
    /// server gave no container is listed with this one, so a qualifier that
    /// still misses can be corrected from the listing.
    pub fn set_nesting(&mut self, nesting: Option<String>) {
        if self.candidate.info.container_name.is_none() {
            self.candidate.info.container_name.clone_from(&nesting);
        }
        self.nesting = nesting;
    }

    /// How closely the qualifier names where the candidate is, or `None`
    /// when it does not name it at all.
    ///
    /// The file is read from the candidate's own path, so it is a fact about
    /// where the symbol is rather than a guess about what it is called: the
    /// module is the file in gleam, QML, Python and a Rust `foo.rs`, and the
    /// enclosing directories are its parents.
    pub fn closeness(&self, qualified: &Qualified) -> Option<Closeness> {
        let directory = match self.path.split_last() {
            Some((_, directory)) => directory,
            None => &[],
        };
        let at = |prefix: &[String]| self.under(prefix, qualified);
        match (at(&self.path), at(directory)) {
            (Some(Under::Place), _) => Some(Closeness::Place),
            (Some(Under::Inside), _) => Some(Closeness::InFile),
            (None, Some(Under::Place)) => Some(Closeness::DirectoryPlace),
            (None, Some(Under::Inside)) => Some(Closeness::InDirectory),
            (None, None) => None,
        }
    }

    /// Whether the file's outline could change what [`Placed::closeness`]
    /// says, given whether the file's text holds the qualifier's last
    /// segment (`mentioned`, asked only when it matters).
    ///
    /// With no container reported, the outline says whether a candidate the
    /// qualifier already reaches by file or directory sits at the top of the
    /// file or inside something there, and — when the text could hold it —
    /// what a candidate the qualifier does not reach sits inside. With a
    /// container reported, an outline's nesting ends with that container, so
    /// only a qualifier ending the same way and not yet a place can gain
    /// from it; a server that reported another container is taken at its
    /// word.
    pub fn asks_outline(&self, qualified: &Qualified, mentioned: impl FnOnce() -> bool) -> bool {
        let Some(last) = qualified.qualifiers.last() else {
            return false;
        };
        match self.candidate.info.container_name.as_deref() {
            None => self.closeness(qualified).is_some() || mentioned(),
            Some(container) => {
                segments(container).last() == Some(&last.as_str())
                    && self.closeness(qualified) != Some(Closeness::Place)
            }
        }
    }

    /// The containers claimed for the candidate, from every source but its
    /// file.
    fn claims(&self, name: &str) -> Vec<String> {
        [
            self.candidate.info.container_name.clone(),
            self.nesting.clone(),
            advertised_container(&self.candidate.info.name, name),
        ]
        .into_iter()
        .flatten()
        .collect()
    }

    /// How the qualifier ends `prefix` followed by the candidate's place in
    /// the file. A claimed container alone counts as a place, since every
    /// suffix of it is a suffix of the prefix followed by it. An empty
    /// prefix is no place of its own.
    fn under(&self, prefix: &[String], qualified: &Qualified) -> Option<Under> {
        let ends = |container: &str| container_matches(Some(container), &qualified.qualifiers);
        let claims = self.claims(&qualified.name);
        let prefix = (!prefix.is_empty()).then(|| prefix.join("::"));
        let placed = claims.iter().any(|claim| {
            ends(claim)
                || prefix
                    .as_ref()
                    .is_some_and(|prefix| ends(&format!("{prefix}::{claim}")))
        });
        match prefix {
            _ if placed => Some(Under::Place),
            Some(prefix) if ends(&prefix) && claims.is_empty() => Some(Under::Place),
            Some(prefix) if ends(&prefix) => Some(Under::Inside),
            _ => None,
        }
    }
}

/// How a qualifier ends a file or directory path.
enum Under {
    /// It names the candidate's place under that path.
    Place,
    /// It names the path, and the candidate is inside something there.
    Inside,
}

/// A file's place under the view, as segments: each directory, then the
/// file stem. Only the stem when the file is not under the view.
fn file_path(root: &Path, file: &Path) -> Vec<String> {
    let stem = file
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(str::to_string);
    let directories = file
        .strip_prefix(root)
        .ok()
        .and_then(Path::parent)
        .map(|parent| {
            parent
                .components()
                .filter_map(|component| component.as_os_str().to_str())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    directories.into_iter().chain(stem).collect()
}
