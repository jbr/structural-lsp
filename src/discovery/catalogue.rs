//! What a server is, what it can serve, and how it compares to the
//! alternatives.
//!
//! The second layer of `docs/discovery.md`. A catalogue is
//! **data**: adding a server is an entry, not a build. `Adapter`
//! (`src/lsp/adapter/`) stays what it is — protocol behaviour that differs
//! — and most entries need none.
//!
//! ## Preference is measured, not inherited
//!
//! The order among candidates for one language is a **score**: what the
//! server answered when asked the questions `definition`, `references`
//! and `rename` ask, through structural's own tool layer
//! (`tests/conformance/`).
//! answers a different question — pylsp advertises more capabilities than
//! pyright and is worse on this path
//! (`journal/2026-09-17-conformance-probe-findings.md`).
//!
//! Editor consensus still earns one job: breaking a tie between servers
//! that answered equally well, where it stands in for
//! maintained-and-preferred. The full ordering rule, the two tiebreakers
//! considered and declined, and the measured set per language are in
//! `docs/catalogue.md`.
//!
//! **There is no admission threshold.** A bar would fire on a server that
//! initializes and serves nothing, whose two causes are a bug of ours and
//! a project the operator never set up. Neither is improved by removing
//! the only candidate for a language. A zero score is a message to the
//! caller, not an exclusion.
//!
//! ## Provenance is not decoration
//!
//! A score is a joint measurement of **server and client at a revision**:
//! four of the defects the first conformance run found were ours, and ty
//! went 2/7 to 5/7 when structural stopped misreading a legal wide range.
//! So a score that cannot say what produced it is an assertion, and
//! [`Score`] cannot be constructed without a [`Measurement`].

use std::collections::BTreeMap;

/// A language a server serves, and the extensions that identify it.
///
/// Owned rather than `&'static str` throughout this module: a catalogue is
/// data, and entries arrive from a generated table and from
/// `structural.toml` alike. A borrowed entry could not express the second
/// (`docs/discovery.md`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Serves {
    pub language: String,
    pub extensions: Vec<String>,
}

impl Serves {
    /// A language whose extensions the catalogue already knows.
    pub fn known(language: &str) -> Option<Self> {
        Some(Self {
            language: language.to_string(),
            extensions: extensions_for_language(language)?
                .iter()
                .map(|e| e.to_string())
                .collect(),
        })
    }

    pub fn new(language: &str, extensions: &[&str]) -> Self {
        Self {
            language: language.to_string(),
            extensions: extensions.iter().map(|e| e.to_string()).collect(),
        }
    }
}

/// Whether a field was read from a source or derived from one.
///
/// Carried so a heuristic cannot silently become a fact: the Emacs
/// inventories' extensions were largely matched by name against another
/// editor's table, and a plan that chose a server on such an extension
/// says so (`reference/editor-inventories.md`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    /// The source stated it.
    Exact,
    /// Derived from something the source stated.
    Inferred,
}

/// Where an entry came from, and how sure each part of it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Provenance {
    /// The inventory or measurement this entry was built from.
    pub source: String,
    /// How the extensions in [`Entry::serves`] were arrived at.
    pub extensions: Confidence,
}

/// The run that produced a score.
///
/// Every field is required. The structural revision matters as much as the
/// server's version, because the score measures the pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Measurement {
    pub server_version: String,
    pub structural_revision: String,
    pub fixture_revision: String,
    /// ISO date of the run.
    pub date: String,
}

/// What a server answered, and what produced the number.
///
/// `answered` of `probes`, not a ratio: which probes failed is the
/// finding, and an average would hide it (`tests/conformance/report.rs`).
/// The per-probe outcomes live with the checked-in scorecard rather than
/// here; this is the ordering key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Score {
    pub answered: u8,
    pub probes: u8,
    pub measurement: Measurement,
}

/// A server: how to launch it, what it serves, and how it ranks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub name: String,
    /// argv, executable first.
    pub command: Vec<String>,
    pub serves: Vec<Serves>,
    /// What this server answered, when it has been measured. `None` is
    /// **unmeasured**, which is not the same as measured badly — see
    /// [`Entry::rank`] for how the order treats the difference.
    pub score: Option<Score>,
    /// How many of the editor inventories in
    /// `tests/conformance/consensus.json` ship this server. The tiebreak,
    /// never the order (`docs/catalogue.md`).
    pub consensus: u16,
    pub provenance: Provenance,
}

/// An entry's place in the order, greatest first.
///
/// The three keys, stated because determinism is asserted against a stated
/// order rather than against a second run (`warm::first_served_file`):
///
/// 1. **score answered**, descending;
/// 2. **consensus**, descending;
/// 3. **name**, ascending — terminal, so the order is total and two
///    entries never depend on which the catalogue happened to list first.
///
/// An unmeasured entry ranks as **zero answered**, deliberately: the whole
/// premise is preferring what we verified answers these questions, so an
/// unverified server does not outrank a verified one, and the way to
/// promote it is to measure it. It still wins when it is the only
/// candidate, because selection proposes every candidate it has and
/// availability settles the rest.
type Rank<'a> = (std::cmp::Reverse<u8>, std::cmp::Reverse<u16>, &'a str);

impl Entry {
    pub fn rank(&self) -> Rank<'_> {
        (
            std::cmp::Reverse(self.score.as_ref().map_or(0, |score| score.answered)),
            std::cmp::Reverse(self.consensus),
            &self.name,
        )
    }

    /// Whether this entry serves `language`.
    pub fn serves_language(&self, language: &str) -> bool {
        self.serves.iter().any(|s| s.language == language)
    }

    /// The extensions this entry claims for `language`.
    pub fn extensions_for(&self, language: &str) -> &[String] {
        self.serves
            .iter()
            .find(|s| s.language == language)
            .map_or(&[], |s| s.extensions.as_slice())
    }

    /// An entry with no score and no consensus, for a server named by
    /// configuration rather than found in the catalogue.
    pub fn configured(name: &str, command: &[String], serves: Vec<Serves>) -> Self {
        Self {
            name: name.to_string(),
            command: command.to_vec(),
            serves,
            score: None,
            consensus: 0,
            provenance: Provenance {
                source: format!("{} entry", crate::config::FILE_NAME),
                extensions: Confidence::Exact,
            },
        }
    }
}

/// The languages this catalogue knows, and the extensions identifying
/// each — the single place that table lives.
///
/// `config::default_extensions` reads it, so a language spelled here needs
/// no `extensions` in `structural.toml`.
pub const LANGUAGES: &[(&str, &[&str])] = &[
    ("bash", &["sh"]),
    ("c", &["c", "h"]),
    ("cpp", &["cpp", "cc", "cxx", "hpp", "hh"]),
    ("elm", &["elm"]),
    ("erlang", &["erl", "hrl"]),
    ("gleam", &["gleam"]),
    ("go", &["go"]),
    ("java", &["java"]),
    ("javascript", &["js", "jsx", "mjs", "cjs"]),
    ("kotlin", &["kt", "kts"]),
    ("python", &["py", "pyi"]),
    ("qml", &["qml"]),
    ("r", &["r"]),
    ("ruby", &["rb"]),
    ("rust", &["rs"]),
    ("swift", &["swift"]),
    ("typescript", &["ts", "tsx", "mts", "cts"]),
    ("zig", &["zig"]),
];

/// The extensions identifying `language`, or `None` for a language this
/// catalogue does not know.
pub fn extensions_for_language(language: &str) -> Option<&'static [&'static str]> {
    LANGUAGES
        .iter()
        .find(|(id, _)| *id == language)
        .map(|(_, extensions)| *extensions)
}

/// The servers shipped with structural today: every measured server of
/// every language the harness covers, generated from the checked-in
/// evidence by `src/bin/catalogue.rs`. A number invented here would be
/// exactly the unverified import the plan refuses; regenerating from the
/// evidence is how the table grows
/// ([`generated.rs`](generated.rs), `docs/catalogue.md`).
fn built_in_entries() -> Vec<Entry> {
    generated::entries()
}

/// A set of entries, indexed by name.
///
/// Built from a slice so a fixture catalogue and the shipped one are the
/// same type: the tests that assert ordering and fallback run against
/// catalogues they construct, which is what lets them exercise cases the
/// shipped catalogue does not yet contain
/// (`docs/discovery.md`).
#[derive(Debug, Clone, Default)]
pub struct Catalogue {
    entries: BTreeMap<String, Entry>,
}

impl Catalogue {
    pub fn new(entries: Vec<Entry>) -> Self {
        Self {
            entries: entries
                .into_iter()
                .map(|entry| (entry.name.clone(), entry))
                .collect(),
        }
    }

    /// The catalogue structural ships.
    pub fn built_in() -> Self {
        Self::new(built_in_entries())
    }

    /// Add or replace an entry, for the override layer.
    pub fn insert(&mut self, entry: Entry) {
        self.entries.insert(entry.name.clone(), entry);
    }

    /// Remove an entry, for a `disabled` override.
    pub fn remove(&mut self, name: &str) -> Option<Entry> {
        self.entries.remove(name)
    }

    pub fn get(&self, name: &str) -> Option<&Entry> {
        self.entries.get(name)
    }

    /// Every entry, in rank order. Iteration starts from a [`BTreeMap`],
    /// so the input's order cannot leak into the output even before the
    /// sort.
    pub fn ranked(&self) -> Vec<&Entry> {
        let mut entries: Vec<&Entry> = self.entries.values().collect();
        entries.sort_by_key(|entry| entry.rank());
        entries
    }

    /// The entries serving `language`, in rank order.
    pub fn candidates_for(&self, language: &str) -> Vec<&Entry> {
        self.ranked()
            .into_iter()
            .filter(|entry| entry.serves_language(language))
            .collect()
    }

    /// Every language any entry serves, sorted.
    pub fn languages(&self) -> Vec<&str> {
        let mut languages: Vec<&str> = self
            .entries
            .values()
            .flat_map(|entry| entry.serves.iter().map(|s| s.language.as_str()))
            .collect();
        languages.sort_unstable();
        languages.dedup();
        languages
    }
}

mod generated;

#[cfg(test)]
mod tests;
