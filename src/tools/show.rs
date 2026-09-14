//! `show` — resolve a symbol by bare name and return its definition's
//! decorated source, not just a location.
//!
//! Identity, not names: `workspace/symbol` only *nominates*. Every exact
//! candidate is sent to `textDocument/definition`, and the results are
//! deduplicated by target — hits that navigate to the same place are one
//! thing; hits that navigate to different places are listed as ambiguous,
//! with their kinds, paths and signatures, and nothing is picked.
//!
//! Correctness order: every owned document is synced from disk, then the
//! call waits until each queried server has no outstanding work, then
//! queries. Each file the answer depends on is synced before the query that
//! reads it, and every range a server returns is checked against the text
//! just read — the symbol's name must sit there. A mismatch is an error,
//! never a retry and never a body served from a stale range.
//!
//! Servers: a `path` naming a file goes to the server for its language;
//! otherwise every running server is asked and the candidates are merged.
//! Bodies come from the server's `documentSymbol` extent, widened over
//! leading decorations by tree-sitter where a grammar exists.

use crate::lsp::definition::{
    Candidate, DefinitionSet, DefinitionTarget, Identity, Partition, Resolution, group_by_target,
    parse_definition_result, partition, verify_name_at,
};
use crate::lsp::outline::{self, Extent};
use crate::lsp::positions::position_to_offset;
use crate::lsp::server::LspServer;
use crate::lsp::shape;
use crate::state::State;
use anyhow::{Context, Result, bail};
use lsp_types::{Location, SymbolKind};
use mcplease::{
    traits::{Tool, ToolMeta},
    types::{RequestContext, ToolAnnotations},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

mod render;
use render::*;

/// Upper bound on the returned item body. Omission is noted, never silent;
/// pagination is not implemented in this slice.
const BODY_BUDGET_BYTES: usize = 12_000;

/// Entries listed before "… N more".
const MAX_LISTED: usize = 20;

/// Show a code definition by its bare name: the item's source, including
/// its doc comments and attributes, not just a location.
///
/// Candidates come from the language servers' fuzzy symbol index (not
/// proven exhaustive); only exact-name matches resolve, and several distinct
/// definitions are listed rather than picked. The answer is current: owned
/// files are re-synced from disk and the servers are waited for before every
/// query. `path` scopes to a file or directory under the workspace root,
/// which is fixed at server start.
#[derive(Serialize, Deserialize, Debug, JsonSchema, clap::Args)]
#[serde(rename = "show")]
#[group(skip)]
pub struct Show {
    /// Exact bare symbol name (e.g. `Config`, `choose`). Qualified paths
    /// (`Config::new`) are rejected; pass the bare name.
    pub symbol: String,

    /// A file or directory under the workspace root, relative to it, to
    /// narrow candidates to. A file also picks the language server for it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

impl ToolMeta for Show {
    fn title() -> Option<&'static str> {
        Some("Show a code definition")
    }

    fn annotations() -> Option<ToolAnnotations> {
        Some(ToolAnnotations {
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            // The tool touches only the workspace root it was started with.
            open_world_hint: Some(false),
            ..Default::default()
        })
    }
}

/// What the filters excluded, carried to the listings.
#[derive(Clone, Copy)]
struct Exclusions {
    out_of_root: usize,
    out_of_scope: usize,
}

impl Tool<State> for Show {
    type Output = String;

    fn execute(self, state: &mut State, _context: &RequestContext) -> Result<Self::Output> {
        let root = state.root().to_path_buf();
        let symbol = validate_symbol(&self.symbol)?;
        let scope = self
            .path
            .as_deref()
            .map(|user_path| resolve_scope(&root, user_path))
            .transpose()?;
        let selected = state.select(scope.as_deref())?;

        let (exact, fuzzy, exclusions) =
            discover(state, &selected, &symbol, &root, scope.as_deref())?;
        if exact.is_empty() {
            return Ok(if fuzzy.is_empty() {
                render_not_found(&symbol, exclusions)
            } else {
                let labels = |server: &str, kind: SymbolKind| kind_label(state, server, kind);
                render_fuzzy(&root, &symbol, &fuzzy, &labels, exclusions)
            });
        }

        // Nomination is not identity: ask each exact candidate's server where
        // it actually goes, then group by destination.
        let mut found = Vec::with_capacity(exact.len());
        for candidate in exact {
            let set = definition_of(state, &root, &symbol, &candidate)?;
            found.push((candidate, set));
        }
        let mut identities = group_by_target(found);
        if identities.len() != 1 {
            let entries = ambiguous_entries(state, &identities);
            return Ok(render_ambiguous(&root, &symbol, &entries, exclusions));
        }
        let identity = identities.pop().expect("exactly one");
        match identity.target.clone() {
            Some(target) => render_definition(state, &root, &symbol, &identity, &target),
            None => Ok(render_null_definition(
                &root,
                &symbol,
                &identity.server,
                &identity.candidates[0].info.location,
            )),
        }
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

/// The single identity's body: own the target's file, check its range,
/// shape, and render with any notes.
fn render_definition(
    state: &mut State,
    root: &Path,
    symbol: &str,
    identity: &Identity,
    target: &DefinitionTarget,
) -> Result<String> {
    let server = server_named(state, &identity.server)?;
    let encoding = server.encoding();
    let raw_path = candidate_path_of(target)?;
    let target_path = std::fs::canonicalize(&raw_path).unwrap_or(raw_path);
    let synced = server.sync(&target_path)?;
    let text = synced.text;
    verify_name_at(&text, target.selection_range, symbol, encoding).with_context(|| {
        format!(
            "{}'s definition target for '{symbol}' ({}) does not match the file on disk; the \
             server's view was stale — retry after it re-indexes",
            identity.server,
            location_line(root, &target.uri, target.selection_range.start)
        )
    })?;
    let language = server.language_id(&target_path);
    let (extent, body, mut notes) = shape_body(server, &synced.uri, &text, target, &language)?;
    if !target_path.starts_with(root) {
        notes.push(format!(
            "definition is outside the workspace root: {}",
            target_path.display()
        ));
    }
    let origin = &identity.candidates[0];
    let kind = extent.as_ref().map_or(origin.info.kind, |extent| extent.kind);
    let name = extent
        .as_ref()
        .map_or(origin.info.name.as_str(), |extent| extent.name.as_str());
    let location = Location {
        uri: target.uri.clone(),
        range: target.selection_range,
    };
    Ok(render_resolved(
        root,
        server.adapter().kind_label(kind),
        name,
        &location,
        &body,
        &notes,
    ))
}

/// The item's extent and body, and notes only when it did not come the
/// normal way.
///
/// Normal: the server's `documentSymbol` gives the extent of the innermost
/// symbol at the definition, and tree-sitter widens its start over the
/// leading doc comments, attributes or decorators the language keeps
/// outside the item (nothing, for a language without a grammar here).
/// Fallback, said in a note: with no extent from the server, the Rust
/// grammar's own item search, or else the server's definition range as-is.
/// Never a guessed item.
fn shape_body(
    server: &LspServer,
    uri: &str,
    text: &str,
    target: &DefinitionTarget,
    language: &str,
) -> Result<(Option<Extent>, String, Vec<String>)> {
    let encoding = server.encoding();
    // Ordered after the sync that precedes this call, so the outline is of
    // the text we read.
    let outline = server.document_symbol(uri)?;
    let extent = match &outline {
        Some(result) => outline::innermost_at(result, target.selection_range.start)
            .with_context(|| {
                format!(
                    "{} returned an unusable documentSymbol result",
                    server.name()
                )
            })?,
        None => None,
    };
    if let Some(extent) = extent {
        let start = position_to_offset(text, extent.range.start, encoding)
            .context("the documentSymbol range starts outside the file as read from disk")?;
        let end = position_to_offset(text, extent.range.end, encoding)
            .context("the documentSymbol range ends outside the file as read from disk")?;
        if start <= end {
            let start = shape::extend_start(language, text, start)?;
            let (body, truncated) = budget(&text[start..end]);
            return Ok((Some(extent), body, truncated.into_iter().collect()));
        }
    }

    let why = match outline {
        None => format!("{} does not support documentSymbol", server.name()),
        Some(_) => format!(
            "{}'s documentSymbol has no symbol at the definition",
            server.name()
        ),
    };
    let offset = position_to_offset(text, target.selection_range.start, encoding)
        .context("definition target range is not inside the target file as read from disk")?;
    let shaped = if language == "rust" {
        shape::item_at(text, offset)?
    } else {
        None
    };
    let (raw, how) = match shaped {
        Some(item) => (
            item.decorated_text(text).to_string(),
            "item found by the Rust grammar instead",
        ),
        None => (
            fallback_body(text, target, encoding).unwrap_or_default(),
            "the server's definition range is shown as-is",
        ),
    };
    let (body, truncated) = budget(&raw);
    let mut notes = vec![format!("{why}; {how}")];
    notes.extend(truncated);
    Ok((None, body, notes))
}

/// Display data for each distinct definition: kind, name, location, and the
/// signature its server's outline gives (best effort — a listing is not
/// worth failing over a signature).
fn ambiguous_entries(state: &mut State, identities: &[Identity]) -> Vec<AmbiguousEntry> {
    identities
        .iter()
        .map(|identity| {
            let origin = &identity.candidates[0];
            let (location, signature) = match &identity.target {
                Some(target) => (
                    Location {
                        uri: target.uri.clone(),
                        range: target.selection_range,
                    },
                    signature_of(state, &identity.server, target),
                ),
                None => (origin.info.location.clone(), None),
            };
            AmbiguousEntry {
                kind_label: kind_label(state, &identity.server, origin.info.kind),
                name: origin.info.name.clone(),
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
            !line.is_empty() && !line.starts_with("//") && !line.starts_with('#') && !line.starts_with('@')
        })
        .map(str::to_string)
}

fn server_named<'a>(state: &'a mut State, name: &str) -> Result<&'a mut LspServer> {
    let index = state
        .index_of(name)
        .with_context(|| format!("{name} is no longer running"))?;
    Ok(state.server_mut(index))
}

fn kind_label(state: &State, server: &str, kind: SymbolKind) -> &'static str {
    match state.index_of(server) {
        Some(index) => state.server(index).adapter().kind_label(kind),
        None => crate::lsp::adapter::default_kind_label(kind),
    }
}

/// The filesystem path of a definition target, for reading and syncing.
fn candidate_path_of(target: &DefinitionTarget) -> Result<PathBuf> {
    crate::lsp::definition::candidate_path(&target.uri).ok_or_else(|| {
        anyhow::anyhow!(
            "definition target URI {} is not a file path",
            target.uri.as_str()
        )
    })
}

/// A bare name: non-empty, and none of the qualification punctuation. A
/// qualified path cannot be validated by bare-name exact matching (the
/// server's `containerName` is display metadata, not a language-level path),
/// so it is rejected rather than falsely resolved.
fn validate_symbol(symbol: &str) -> Result<String> {
    let trimmed = symbol.trim();
    if trimmed.is_empty() {
        bail!("`symbol` must be a non-empty bare name (e.g. `Config`)");
    }
    if trimmed.contains("::") || trimmed.contains('.') || trimmed.contains('/') {
        bail!(
            "qualified paths (`{trimmed}`) are not supported: only bare symbol names are \
             matched. Pass the bare name — e.g. `new` for `Config::new`."
        );
    }
    Ok(trimmed.to_string())
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

#[cfg(test)]
mod tests;
