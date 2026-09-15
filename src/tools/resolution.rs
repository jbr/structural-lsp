//! The resolution pipeline every tool shares: a target (a name, or an
//! anchor in a file) to one *identity*, or to a listing that says why
//! there is not one.
//!
//! Identity, not names: `workspace/symbol` only *nominates*. Every exact
//! candidate is sent to `textDocument/definition`, and the results are
//! deduplicated by target — hits that navigate to the same place are one
//! thing; hits that navigate to different places are listed as ambiguous,
//! with their kinds, paths and signatures, and nothing is picked. A
//! qualified name (`Config::new`) filters the exact candidates by the
//! container the server reported ([`qualify`]); an anchor skips the index
//! and asks `definition` at the focus (the `anchored` module).
//!
//! Correctness order: every owned document is synced from disk, then the
//! call waits until each queried server has no outstanding work, then
//! queries. Each file the answer depends on is synced before the query that
//! reads it, and every range a server returns is checked against the text
//! just read — the symbol's name must sit there. A mismatch is an error,
//! never a retry and never an answer served from a stale range.
//!
//! Servers: a `path` naming a file goes to the server for its language;
//! otherwise every running server is asked and the candidates are merged.

use crate::lsp::definition::{
    Candidate, DefinitionSet, DefinitionTarget, Identity, Origin, Partition, Resolution,
    group_by_target, parse_definition_result, partition, verify_name_at,
};
use crate::lsp::outline::{self, Extent};
use crate::lsp::positions::position_to_offset;
use crate::lsp::server::LspServer;
use crate::state::State;
use crate::tools::listing::{
    AmbiguousEntry, location_line, render_ambiguous, render_fuzzy, render_not_found,
    render_null_definition, render_unqualified,
};
use anyhow::{Context, Result, bail};
use lsp_types::SymbolKind;
use serde_json::Value;
use std::path::{Path, PathBuf};

mod anchored;
pub mod qualify;

/// What the filters excluded, carried to the listings.
#[derive(Clone, Copy, Debug, Default)]
pub struct Exclusions {
    pub out_of_root: usize,
    pub out_of_scope: usize,
}

/// What a tool was asked to resolve, as the user gave it.
#[derive(Debug, Clone, Copy, Default)]
pub struct Target<'a> {
    /// A bare or qualified name.
    pub symbol: Option<&'a str>,
    /// A scope under the root; the file an anchor is in.
    pub path: Option<&'a str>,
    /// Quoted source text that occurs once in `path`.
    pub anchor: Option<&'a str>,
    /// The token within the anchor that is meant; the whole anchor when
    /// absent.
    pub focus: Option<&'a str>,
}

/// The outcome of resolving a target.
pub enum Resolved {
    /// Not found, fuzzy only, ambiguous, or a lone hit with no definition:
    /// the answer is this listing, already rendered.
    Listed(String),
    /// Exactly one definition target.
    One(Box<Found>),
}

/// The identity and the definition target it resolved to.
pub struct Found {
    pub identity: Identity,
    pub target: DefinitionTarget,
    /// The name that sits at the target: the bare name asked for, or what
    /// the anchor's focus resolved to.
    pub name: String,
}

/// Resolve `target` under the fixed root. Argument errors are errors; every
/// other outcome is a [`Resolved`].
pub fn resolve_target(state: &mut State, target: Target<'_>) -> Result<Resolved> {
    let root = state.root().to_path_buf();
    let scope = target
        .path
        .map(|user_path| resolve_scope(&root, user_path))
        .transpose()?;
    match (target.symbol, target.anchor) {
        (Some(_), Some(_)) => bail!("give either `symbol` or `anchor`, not both"),
        (None, None) => bail!(
            "give a `symbol` (a name such as `Config` or `Config::new`) or an `anchor` (source \
             text quoted from the file named by `path`)"
        ),
        (Some(symbol), None) => {
            if target.focus.is_some() {
                bail!("`focus` selects within an `anchor`; with `symbol` it has no meaning");
            }
            resolve_symbol(state, &root, symbol, scope)
        }
        (None, Some(anchor)) => anchored::from_anchor(state, &root, scope, anchor, target.focus),
    }
}

/// Resolve a (possibly qualified) name.
fn resolve_symbol(
    state: &mut State,
    root: &Path,
    symbol: &str,
    scope: Option<PathBuf>,
) -> Result<Resolved> {
    let qualified = qualify::parse(symbol)?;
    let display = qualified.display();
    let selected = state.select(scope.as_deref())?;

    let (exact, fuzzy, exclusions) =
        discover(state, &selected, &qualified.name, root, scope.as_deref())?;
    let (exact, unqualified) = qualify::filter(exact, &qualified.qualifiers);
    if exact.is_empty() {
        let labels = |server: &str, kind: SymbolKind| kind_label(state, server, kind);
        return Ok(Resolved::Listed(if !unqualified.is_empty() {
            render_unqualified(root, &qualified, &unqualified, &labels)
        } else if fuzzy.is_empty() {
            render_not_found(&display, exclusions)
        } else {
            render_fuzzy(root, &display, &fuzzy, &labels, exclusions)
        }));
    }

    // Nomination is not identity: ask each exact candidate's server where
    // it actually goes, then group by destination.
    let mut found = Vec::with_capacity(exact.len());
    for candidate in exact {
        let set = definition_of(state, root, &qualified.name, &candidate)?;
        found.push((candidate, set));
    }
    finish(state, root, group_by_target(found), &display, exclusions)
}

/// One identity is the resolution; several are the ambiguous listing; one
/// with no definition is stated.
fn finish(
    state: &mut State,
    root: &Path,
    mut identities: Vec<Identity>,
    display: &str,
    exclusions: Exclusions,
) -> Result<Resolved> {
    if identities.len() != 1 {
        let entries = ambiguous_entries(state, &identities);
        return Ok(Resolved::Listed(render_ambiguous(
            root, display, &entries, exclusions,
        )));
    }
    let identity = identities.pop().expect("exactly one");
    match identity.target.clone() {
        Some(target) => Ok(Resolved::One(Box::new(Found {
            name: identity.origin.name.clone(),
            identity,
            target,
        }))),
        None => Ok(Resolved::Listed(render_null_definition(
            root,
            display,
            &identity.server,
            &identity.origin.location,
        ))),
    }
}

/// Sync every selected server's owned documents, wait for each, and merge
/// their `workspace/symbol` candidates: exact matches, fuzzy matches, and
/// what the root and scope filters excluded.
fn discover(
    state: &mut State,
    selected: &[usize],
    symbol: &str,
    root: &Path,
    scope: Option<&Path>,
) -> Result<(Vec<Candidate>, Vec<Candidate>, Exclusions)> {
    // The wait is unbounded except by the inactivity backstop: a busy server
    // is waited for, a silent one is an error.
    for &index in selected {
        let server = state.server_mut(index);
        server.sync_owned()?;
        server.wait_ready()?;
    }
    let mut merged = Partition::default();
    for &index in selected {
        let server = state.server(index);
        let infos = server.workspace_symbol(symbol)?;
        merged.absorb(partition(symbol, server.name(), infos, root, scope));
    }
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
    Ok((exact, fuzzy, exclusions))
}

/// Own the candidate's file, check the index's range against the text just
/// read, wait for the server, and ask where the candidate's definition is.
/// A range that no longer names the symbol is a stale index, reported
/// rather than navigated from.
fn definition_of(
    state: &mut State,
    root: &Path,
    symbol: &str,
    candidate: &Candidate,
) -> Result<DefinitionSet> {
    let server = server_named(state, &candidate.server)?;
    let file = candidate
        .canonical_path
        .clone()
        .ok_or_else(|| anyhow::anyhow!("candidate URI is not a readable file path"))?;
    let range = candidate.info.location.range;
    let synced = server.sync(&file)?;
    verify_name_at(&synced.text, range, symbol, server.encoding()).with_context(|| {
        format!(
            "{}'s location for '{symbol}' ({}) does not match the file on disk; the server's \
             view was stale — retry after it re-indexes",
            candidate.server,
            location_line(root, &candidate.info.location.uri, range.start)
        )
    })?;
    // The didOpen/didChange above precedes this request on the wire, so the
    // server answers against the text we read. Wait for no outstanding work
    // again: the sync may have started some.
    server.wait_ready()?;
    let raw = server.definition(&synced.uri, range.start)?;
    parse_definition_result(raw).with_context(|| {
        format!(
            "{} returned an unrecognizable definition result",
            candidate.server
        )
    })
}

/// Own the identity's target file and check that the name sits at the
/// target range; the text read is what every later position converts
/// against. Returns the canonical path and the synced document.
pub fn sync_target(
    state: &mut State,
    root: &Path,
    found: &Found,
) -> Result<(PathBuf, crate::lsp::server::Synced)> {
    let Found {
        identity,
        target,
        name,
    } = found;
    let server = server_named(state, &identity.server)?;
    let raw_path = candidate_path_of(target)?;
    let target_path = std::fs::canonicalize(&raw_path).unwrap_or(raw_path);
    let synced = server.sync(&target_path)?;
    verify_name_at(
        &synced.text,
        target.selection_range,
        name,
        server.encoding(),
    )
    .with_context(|| {
        format!(
            "{}'s definition target for '{name}' ({}) does not match the file on disk; \
                 the server's view was stale — retry after it re-indexes",
            identity.server,
            location_line(root, &target.uri, target.selection_range.start)
        )
    })?;
    Ok((target_path, synced))
}

/// The header's kind word and name for a resolved target, and the outline
/// extent when the outline symbol at the target *is* the target.
pub struct Described {
    pub kind_label: Option<&'static str>,
    pub name: String,
    /// The target's own outline symbol: present only when the outline's
    /// selection range is the target's (or, for a flat outline without
    /// one, the names agree). A local variable is in no outline, so its
    /// header carries no kind word and its enclosing item is never
    /// mistaken for it.
    pub extent: Option<Extent>,
}

/// Describe `found` from the outline of its file (`None` when the server
/// gave none).
pub fn describe(server: &LspServer, outline: Option<&Value>, found: &Found) -> Result<Described> {
    let extent = match outline {
        Some(outline) => outline::innermost_at(outline, found.target.selection_range.start)
            .with_context(|| {
                format!(
                    "{} returned an unusable documentSymbol result",
                    server.name()
                )
            })?,
        None => None,
    };
    let confirmed = extent.filter(|extent| match extent.selection_range {
        Some(range) => range == found.target.selection_range,
        None => extent.name == found.name,
    });
    Ok(match confirmed {
        Some(extent) => Described {
            kind_label: Some(server.adapter().kind_label(extent.kind)),
            name: extent.name.clone(),
            extent: Some(extent),
        },
        None => Described {
            kind_label: found
                .identity
                .origin
                .kind
                .map(|kind| server.adapter().kind_label(kind)),
            name: found.name.clone(),
            extent: None,
        },
    })
}

/// Display data for each distinct definition: kind, name, location, and the
/// signature its server's outline gives (best effort — a listing is not
/// worth failing over a signature).
fn ambiguous_entries(state: &mut State, identities: &[Identity]) -> Vec<AmbiguousEntry> {
    identities
        .iter()
        .map(|identity| {
            let origin: &Origin = &identity.origin;
            let (location, signature) = match &identity.target {
                Some(target) => (
                    lsp_types::Location {
                        uri: target.uri.clone(),
                        range: target.selection_range,
                    },
                    signature_of(state, &identity.server, target),
                ),
                None => (origin.location.clone(), None),
            };
            AmbiguousEntry {
                kind_label: origin
                    .kind
                    .map(|kind| kind_label(state, &identity.server, kind)),
                name: origin.name.clone(),
                server: identity.server.clone(),
                location,
                signature,
            }
        })
        .collect()
}

/// The outline's `detail` for the symbol at `target`, else the first line of
/// its extent. `None` when anything on the way fails.
fn signature_of(state: &mut State, server: &str, target: &DefinitionTarget) -> Option<String> {
    let server = server_named(state, server).ok()?;
    let path = candidate_path_of(target).ok()?;
    let synced = server.sync(&path).ok()?;
    let outline = server.document_symbol(&synced.uri).ok()??;
    let extent = outline::innermost_at(&outline, target.selection_range.start).ok()??;
    if let Some(detail) = extent
        .detail
        .as_deref()
        .map(str::trim)
        .filter(|detail| !detail.is_empty())
    {
        return Some(detail.to_string());
    }
    // The extent may begin with doc comments or attributes; the signature
    // is the first line of code.
    let start = position_to_offset(&synced.text, extent.range.start, server.encoding()).ok()?;
    synced.text[start..]
        .lines()
        .map(str::trim)
        .find(|line| {
            !line.is_empty()
                && !line.starts_with("//")
                && !line.starts_with('#')
                && !line.starts_with('@')
        })
        .map(str::to_string)
}

pub fn server_named<'a>(state: &'a mut State, name: &str) -> Result<&'a mut LspServer> {
    let index = state
        .index_of(name)
        .with_context(|| format!("{name} is no longer running"))?;
    Ok(state.server_mut(index))
}

pub fn kind_label(state: &State, server: &str, kind: SymbolKind) -> &'static str {
    match state.index_of(server) {
        Some(index) => state.server(index).adapter().kind_label(kind),
        None => crate::lsp::adapter::default_kind_label(kind),
    }
}

/// The filesystem path of a definition target, for reading and syncing.
pub fn candidate_path_of(target: &DefinitionTarget) -> Result<PathBuf> {
    crate::lsp::definition::candidate_path(&target.uri).ok_or_else(|| {
        anyhow::anyhow!(
            "definition target URI {} is not a file path",
            target.uri.as_str()
        )
    })
}

/// Canonicalize a user scope under the fixed root: it must exist and stay
/// inside the root. Never changes a server's root.
fn resolve_scope(root: &Path, user_path: &str) -> Result<PathBuf> {
    let joined = root.join(user_path);
    let canonical = joined.canonicalize().with_context(|| {
        format!("scope path `{user_path}` does not exist under the workspace root")
    })?;
    if !(canonical == root || canonical.starts_with(root)) {
        bail!(
            "scope path `{user_path}` escapes the workspace root ({}); `path` is only a scope \
             inside it",
            root.display()
        );
    }
    Ok(canonical)
}
