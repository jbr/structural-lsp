//! Evidence + catalogue + overrides → plans, with the reasons for each.
//!
//! The third layer of `backlog/discovery/selection.md`, and the seam that
//! matters: **selection is a function, not a side effect.** It starts no
//! process, watches no filesystem and takes no lock. It returns a value,
//! and the value is what the tests assert — which is what lets every
//! Done-when clause of that unit fail on a fixture tree.
//!
//! A plan is a **proposal**. Whether the server can actually start is
//! acquisition's to find out, and a failure there is reported rather than
//! silently dropped; walking to the next alternative is milestone C. What
//! this layer owes is that the proposal is deterministic and says why.
//!
//! ## Precedence
//!
//! Explicit and ordered, highest first:
//!
//! 1. an **override** — `structural.toml` names a server, so it is
//!    configured, added, or disabled;
//! 2. the **catalogue** — a known server whose rank was measured;
//! 3. a **guess from evidence** — a language is present and some entry
//!    serves it.
//!
//! ## Determinism
//!
//! Asserted against a **stated** order, not against a second run. Purity
//! means the nondeterminism that can reach selection arrives with its
//! inputs — directory iteration order, and any map whose iteration order
//! is not fixed — so running twice in one process can agree while the
//! property is false (`src/lsp/server/warm.rs` met this exactly). Plans
//! come out sorted by language, then by the rank in
//! [`super::catalogue`]; every collection this module builds is ordered
//! before it is read.

use super::catalogue::{Catalogue, Confidence, Entry};
use super::evidence::Evidence;
use super::roots::Root;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

/// What `structural.toml` says, as a layer over the catalogue rather than
/// as the whole configuration.
///
/// Today the file *is* the configuration (`src/config.rs`); after
/// milestone B it is this. Disabling is the capability that does not exist
/// today at all — a project with no Rust in it cannot currently say so.
#[derive(Debug, Clone, Default)]
pub struct Overrides {
    disabled: BTreeSet<String>,
    added: BTreeMap<String, Entry>,
}

impl Overrides {
    pub fn new() -> Self {
        Self::default()
    }

    /// Take a server out of consideration for this project, whether or not
    /// the catalogue knows it.
    pub fn disable(mut self, name: &str) -> Self {
        self.disabled.insert(name.to_string());
        self
    }

    /// Name a server: add one the catalogue does not know, or replace one
    /// it does. Both spellings of "`structural.toml` says so", which is
    /// why they are one method.
    pub fn configure(mut self, entry: Entry) -> Self {
        self.added.insert(entry.name.clone(), entry);
        self
    }

    pub fn is_disabled(&self, name: &str) -> bool {
        self.disabled.contains(name)
    }

    /// The catalogue with this layer applied: additions replace entries of
    /// the same name, and disabled entries are gone.
    fn applied(&self, catalogue: &Catalogue) -> Catalogue {
        let mut merged = catalogue.clone();
        for entry in self.added.values() {
            merged.insert(entry.clone());
        }
        for name in &self.disabled {
            merged.remove(name);
        }
        merged
    }
}

/// Why a language is believed present, or why a server was chosen for it.
///
/// Reasons are data, not prose, so a test can assert that a plan is right
/// for the *right* reason rather than merely right.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reason {
    /// A project marker above the working directory implies the language.
    Marker { file: String, language: String },
    /// Files carrying one of the language's extensions are in the tree.
    Sources { extension: String, count: usize },
    /// It answered more of the conformance probes than the alternatives.
    HighestScore { answered: u8, probes: u8 },
    /// Nothing else in the catalogue serves this language. Distinct from
    /// [`Reason::HighestScore`] because "it won" and "there was no one
    /// else" are different claims, and only one survives one candidate.
    ///
    /// Its acquisition-time twin — chosen because it was the only
    /// candidate whose command would *start* — belongs to milestone C.
    OnlyCandidate,
    /// Chosen without a measured score, so its rank is the floor.
    Unmeasured,
    /// `structural.toml` named it.
    Configured,
    /// Chosen on extensions that were derived rather than stated. Carried
    /// so a heuristic cannot pass as a fact (`reference/servers-list/`).
    ExtensionsInferred,
}

/// A server proposed for a language, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Plan {
    pub server: String,
    pub language: String,
    /// The project root the server would be started against, and what the
    /// scope of an answer is *not* — narrowing stays the caller's working
    /// directory (`docs/resolution.md`).
    pub root: PathBuf,
    pub command: Vec<String>,
    /// The other candidates for this language, in rank order. What
    /// milestone C walks when this one fails to start.
    pub alternatives: Vec<String>,
    pub why: Vec<Reason>,
}

impl Plan {
    /// One line, for an operator log and for a plan's own report.
    pub fn explain(&self) -> String {
        let why: Vec<String> = self.why.iter().map(Reason::describe).collect();
        format!("{} for {}: {}", self.server, self.language, why.join("; "))
    }
}

impl Reason {
    pub fn describe(&self) -> String {
        match self {
            Reason::Marker { file, language } => format!("`{file}` implies {language}"),
            Reason::Sources { extension, count } => {
                format!("{count} `.{extension}` file{} in the tree", plural(*count))
            }
            Reason::HighestScore { answered, probes } => {
                format!("answered {answered} of {probes} probes, the most of any candidate")
            }
            Reason::OnlyCandidate => "the only candidate for this language".to_string(),
            Reason::Unmeasured => "not yet measured, so ranked at the floor".to_string(),
            Reason::Configured => format!("named in {}", crate::config::FILE_NAME),
            Reason::ExtensionsInferred => {
                "its extensions were inferred rather than stated".to_string()
            }
        }
    }
}

fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

/// A server the catalogue holds that no plan proposes, and why not.
///
/// Kept beside the plans because "irrelevant servers do not start" is only
/// checkable if the reason one was left out is visible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Declined {
    pub server: String,
    pub reason: DeclinedReason,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeclinedReason {
    /// Nothing in the tree suggests any language this server serves.
    NoEvidence { languages: Vec<String> },
    /// `structural.toml` disabled it.
    Disabled,
    /// Another candidate for every language it serves outranked it. It is
    /// still on those plans' `alternatives`.
    Outranked { by: Vec<String> },
}

/// The proposal for one tree: what to start, and what was left out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Selection {
    pub root: Root,
    /// Sorted by language, then by rank. Stated, not incidental.
    pub plans: Vec<Plan>,
    /// Sorted by server name.
    pub declined: Vec<Declined>,
}

impl Selection {
    /// Whether any plan proposes `server`.
    pub fn proposes(&self, server: &str) -> bool {
        self.plans.iter().any(|plan| plan.server == server)
    }

    pub fn declined_reason(&self, server: &str) -> Option<&DeclinedReason> {
        self.declined
            .iter()
            .find(|declined| declined.server == server)
            .map(|declined| &declined.reason)
    }
}

/// Evidence + catalogue + overrides → plans.
///
/// Nothing is started. The returned value is the whole output.
pub fn select(evidence: &Evidence, catalogue: &Catalogue, overrides: &Overrides) -> Selection {
    let catalogue = overrides.applied(catalogue);
    let root = evidence.ancestry.root();

    // Which languages the tree gives evidence for, and what that evidence
    // was. A BTreeMap so the plan order below is the language order and
    // neither depends on how the evidence was gathered.
    let mut evidence_for: BTreeMap<String, Vec<Reason>> = BTreeMap::new();
    for marker in &evidence.ancestry.markers {
        for language in marker.languages {
            evidence_for
                .entry((*language).to_string())
                .or_default()
                .push(Reason::Marker {
                    file: marker.file.to_string(),
                    language: (*language).to_string(),
                });
        }
    }
    for language in catalogue.languages() {
        for entry in catalogue.candidates_for(language) {
            for extension in entry.extensions_for(language) {
                let count = evidence.sources.count(extension);
                if count > 0 {
                    let reason = Reason::Sources {
                        extension: extension.clone(),
                        count,
                    };
                    let reasons = evidence_for.entry(language.to_string()).or_default();
                    if !reasons.contains(&reason) {
                        reasons.push(reason);
                    }
                }
            }
        }
    }

    let mut plans = Vec::new();
    let mut chosen: BTreeSet<String> = BTreeSet::new();
    let mut outranked: BTreeMap<String, Vec<String>> = BTreeMap::new();

    for (language, mut reasons) in evidence_for {
        let candidates = catalogue.candidates_for(&language);
        let Some((winner, rest)) = candidates.split_first() else {
            // Evidence for a language no entry serves. Not a decline —
            // there is no server to decline — and not silently dropped
            // either: `operator-diagnostics.md` owns telling someone.
            continue;
        };

        // One reason per claim: "it won" and "there was no one else" are
        // different, and a lone candidate has not beaten anything.
        if rest.is_empty() {
            reasons.push(Reason::OnlyCandidate);
        } else if let Some(score) = winner.score.as_ref() {
            reasons.push(Reason::HighestScore {
                answered: score.answered,
                probes: score.probes,
            });
        }
        if winner.score.is_none() {
            reasons.push(Reason::Unmeasured);
        }
        if overrides.added.contains_key(&winner.name) {
            reasons.push(Reason::Configured);
        }
        if winner.provenance.extensions == Confidence::Inferred {
            reasons.push(Reason::ExtensionsInferred);
        }

        for loser in rest {
            outranked
                .entry(loser.name.clone())
                .or_default()
                .push(winner.name.clone());
        }
        chosen.insert(winner.name.clone());
        plans.push(Plan {
            server: winner.name.clone(),
            language: language.clone(),
            root: root.path.clone(),
            command: winner.command.clone(),
            alternatives: rest.iter().map(|entry| entry.name.clone()).collect(),
            why: reasons,
        });
    }

    plans.sort_by(|a, b| {
        a.language
            .cmp(&b.language)
            .then_with(|| a.server.cmp(&b.server))
    });

    let declined = declined_from(&catalogue, overrides, &chosen, &outranked);
    Selection {
        root,
        plans,
        declined,
    }
}

/// Every catalogue entry no plan names, with the reason it is absent.
fn declined_from(
    catalogue: &Catalogue,
    overrides: &Overrides,
    chosen: &BTreeSet<String>,
    outranked: &BTreeMap<String, Vec<String>>,
) -> Vec<Declined> {
    let mut declined: Vec<Declined> = Vec::new();
    for name in overrides.disabled.iter() {
        declined.push(Declined {
            server: name.clone(),
            reason: DeclinedReason::Disabled,
        });
    }
    for entry in catalogue.ranked() {
        if chosen.contains(&entry.name) {
            continue;
        }
        let reason = match outranked.get(&entry.name) {
            Some(by) => DeclinedReason::Outranked { by: by.clone() },
            None => DeclinedReason::NoEvidence {
                languages: entry.serves.iter().map(|s| s.language.clone()).collect(),
            },
        };
        declined.push(Declined {
            server: entry.name.clone(),
            reason,
        });
    }
    declined.sort_by(|a, b| a.server.cmp(&b.server));
    declined
}

#[cfg(test)]
mod tests;
