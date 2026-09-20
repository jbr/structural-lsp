//! Where the candidates for a name come from, before any of them is asked
//! where it goes: a named file's own outline, or the servers' symbol index.
//!
//! The index is the fallback rather than the first answer because its pool
//! is bounded and fuzzy-ordered: a file scope that consulted it first could
//! miss a definition the file plainly declares, and the qualifier would
//! then have nothing to narrow (`docs/resolution.md`).

use super::{Exclusions, file_scope};
use crate::lsp::definition::{Candidate, Partition, Resolution, partition};
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
    // The wait is unbounded except by the inactivity backstop: a busy server
    // is waited for, a silent one is an error.
    for &index in selected {
        let server = state.server_mut(index)?;
        server.sync_owned()?;
        server.wait_ready()?;
    }
    let mut merged = Partition::default();
    let mut unindexed: Vec<String> = Vec::new();
    for &index in selected {
        let server = state.server(index)?;
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
        merged.absorb(partition(symbol, server.name(), infos, root, scope));
    }
    // After merging, so a server that advertises the bare name settles it
    // for every server: a decorated name is a fallback, never a second
    // exact match beside a plain one (`docs/resolution.md`).
    merged.promote_decorated(symbol);
    let candidates = merged.into_candidates();
    let exclusions = Exclusions {
        out_of_root: candidates.out_of_root,
        out_of_scope: candidates.out_of_scope,
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
    })
}
