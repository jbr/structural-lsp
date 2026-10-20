//! Where the candidates for a name come from, before any of them is asked
//! where it goes: a named file's own outline, or the servers' symbol index.
//!
//! The index is the fallback rather than the first answer because its pool
//! is bounded and fuzzy-ordered: a file scope that consulted it first could
//! miss a definition the file plainly declares, and the qualifier would
//! then have nothing to narrow (`docs/resolution.md`).

use super::{Exclusions, file_scope};
use crate::lsp::definition::{Candidate, Partition, Resolution, partition};
use crate::tools::listing::Truncated;
use crate::tools::resolution::qualify::advertises;
use crate::workspace::Workspace;
use anyhow::Result;
use std::path::Path;

/// What nomination produced, and what it could not ask.
#[derive(Default)]
pub struct Nominated {
    /// Candidates whose name is the one asked for.
    pub exact: Vec<Candidate>,
    /// Candidates the index offered for a query that matched nothing
    /// exactly.
    pub fuzzy: Vec<Candidate>,
    /// What the root and scope filters removed.
    pub exclusions: Exclusions,
    /// The languages of the selected servers that advertise no workspace
    /// symbol index, so nothing could be asked for them: pylsp, gleam and
    /// qmlls serve documents and no index at all
    /// (`reference/lsp-server-conventions.md`). A miss under one of those
    /// says the symbol is absent, and the two are not the same thing
    /// (`docs/resolution.md`).
    pub unindexed: Vec<String>,
    /// The servers whose answer reached their `workspace/symbol` cap without
    /// naming the symbol: what they hold past the cap is unknown.
    pub truncated: Vec<Truncated>,
}

/// Nominate candidates for the bare name: from a named file's own outline,
/// when `path` names a file and that outline names the symbol; otherwise
/// from the servers' symbol index, narrowed to the scope.
pub fn nominate(
    state: &mut Workspace,
    selected: &[usize],
    symbol: &str,
    root: &Path,
    scope: Option<&Path>,
) -> Result<Nominated> {
    if let Some(file) = scope.filter(|path| path.is_file()) {
        let exact = file_scope::nominate(state, selected, file, symbol)?;
        if !exact.is_empty() {
            // The candidates are the file's own, not a sample of a bounded
            // cross-workspace response, and no scope was consulted that
            // could have excluded a candidate.
            return Ok(Nominated {
                exact,
                ..Nominated::default()
            });
        }
    }
    discover(state, selected, symbol, root, scope)
}

/// Sync every selected server's owned documents, wait for each, and merge
/// their `workspace/symbol` candidates: exact matches, fuzzy matches, what
/// the root and scope filters excluded, and which languages had no index to
/// ask.
fn discover(
    state: &mut Workspace,
    selected: &[usize],
    symbol: &str,
    root: &Path,
    scope: Option<&Path>,
) -> Result<Nominated> {
    // Every push first, then every wait: the waits are unbounded except by
    // the inactivity backstop, so a busy server is waited for and a silent
    // one is an error. A server can still begin work after its wait — while
    // another is waited for, or after its own query is on the wire — and
    // that is the transport's guard: an answer below that arrives while
    // work begun since its write is outstanding is set aside, and the
    // query asked again once the server is ready (docs/correctness.md).
    for &index in selected {
        state.server_mut(index)?.sync_owned()?;
    }
    for &index in selected {
        state.server_mut(index)?.wait_ready()?;
    }
    let mut merged = Partition::default();
    let mut unindexed: Vec<String> = Vec::new();
    let mut truncated: Vec<Truncated> = Vec::new();
    for &index in selected {
        let server = state.server_mut(index)?;
        // The capability is consulted before the request is sent anyway
        // (`LspServer::workspace_symbol` answers an unadvertised provider
        // with nothing); what is recorded here is that the answer was empty
        // for want of an index rather than for want of a symbol.
        if !server.supports("workspaceSymbolProvider") {
            for language in server.adapter().languages() {
                if !unindexed.contains(&language.id) {
                    unindexed.push(language.id);
                }
            }
            continue;
        }
        let infos = server.workspace_symbol(symbol)?;
        // An index that yields is an answer even when every candidate is
        // filtered out later: the server spoke, so a later miss is absence
        // rather than a server that never became ready (docs/resolution.md).
        if !infos.is_empty() {
            server.mark_answered();
        }
        // An answer at the server's cap is a prefix of its matches. One that
        // does not name the symbol says nothing about whether the server
        // holds it; one whose last entry names it may have more of the same
        // name past the cap, since a server that orders by name or by score
        // keeps them together. Either is carried to the answer
        // (docs/resolution.md).
        if let Some(cap) = server.symbol_cap_reached(infos.len()) {
            let names = |info: &lsp_types::SymbolInformation| advertises(&info.name, symbol);
            let named = infos.iter().any(names);
            if !named || infos.last().is_some_and(names) {
                truncated.push(Truncated {
                    server: server.name().to_string(),
                    cap,
                    named,
                });
            }
        }
        merged.absorb(partition(symbol, server.name(), infos, root, scope));
    }
    // After merging, so a server that advertises the bare name settles it
    // for every server: a decorated name is a fallback, never a second
    // exact match beside a plain one (`docs/resolution.md`).
    merged.drop_missing();
    merged.promote_decorated(symbol);
    let candidates = merged.into_candidates();
    let exclusions = Exclusions {
        out_of_root: candidates.out_of_root,
        out_of_scope: candidates.out_of_scope,
        missing: candidates.missing,
    };
    let (exact, fuzzy) = match candidates.resolution {
        Resolution::NotFound => (Vec::new(), Vec::new()),
        Resolution::FuzzyOnly(fuzzy) => (Vec::new(), fuzzy),
        Resolution::ExactOne(candidate) => (vec![*candidate], Vec::new()),
        Resolution::ExactMany(exact) => (exact, Vec::new()),
    };
    Ok(Nominated {
        exact,
        fuzzy,
        exclusions,
        unindexed,
        truncated,
    })
}
