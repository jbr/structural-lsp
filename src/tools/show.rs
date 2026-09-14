//! `show` — resolve a symbol by bare name and return its definition's
//! decorated source, not just a location.
//!
//! Slice boundary (deliberate): bare exact symbol names only, one fixed
//! workspace root, definition only. No anchor/focus targeting, no line:col
//! input, no qualified paths (rejected with guidance — a bare-name exact
//! match is the only resolution that can be honestly validated), no other
//! views. Results separate exact from fuzzy candidates without claiming
//! search completeness, report the resolved selection before the body, and
//! mark omissions.
//!
//! Identity, not names: `workspace/symbol` only *nominates*. Every exact
//! candidate is sent to `textDocument/definition`, and the results are
//! deduplicated by target — hits that navigate to the same place are one
//! thing; hits that navigate to different places are listed as ambiguous,
//! with their kinds and paths, and nothing is picked.
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

use crate::lsp::definition::{
    Candidate, DefinitionSet, DefinitionTarget, Identity, NavigationProvenance, Partition,
    Resolution, group_by_target, parse_definition_result, partition, verify_name_at,
};
use crate::lsp::positions::{Encoding, position_to_offset};
use crate::lsp::server::LspServer;
use crate::lsp::shape;
use crate::state::State;
use anyhow::{Context, Result, bail};
use mcplease::{
    traits::{Tool, ToolMeta},
    types::{RequestContext, ToolAnnotations},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

mod render;
use render::*;

/// Upper bound on the returned item body. Omission is marked, never silent;
/// pagination is not implemented in this slice.
const BODY_BUDGET_BYTES: usize = 12_000;

/// Candidates and definition locations listed before "… N more".
const MAX_LISTED: usize = 20;

/// Resolve a code definition by symbol name; returns the definition's source
/// and documentation, not just a location
///
/// `symbol` must be a bare name (e.g. `Config`, `doubled`) matched exactly;
/// names the server only fuzzy-matches are listed as possible matches and
/// never silently resolved. Optional `path` narrows candidates to a file or
/// directory under the workspace root fixed at server start (candidates
/// outside the root are always excluded, symlink or not) and, for a file,
/// picks the language server for it. The call waits for the servers to
/// finish any outstanding work before answering.
#[derive(Serialize, Deserialize, Debug, JsonSchema, clap::Args)]
#[serde(rename = "show")]
#[group(skip)]
pub struct Show {
    /// Exact bare symbol name (e.g. `Config`). Qualified paths
    /// (`Config::new`) are rejected: they cannot be validated by bare-name
    /// matching in this slice.
    pub symbol: String,

    /// Optional scope: a file or directory under the workspace root, relative
    /// to it. Narrows `workspace/symbol` candidates; never changes a
    /// server's root. Not supported in this slice: `anchor`/`focus`
    /// targeting and line:col input.
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

/// What the filters excluded, carried to every rendering.
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
        let servers_line = state.descriptions();

        // Push disk state for every file the servers delegate to us, then
        // wait until nothing is outstanding on any of them. The wait is
        // unbounded except by the inactivity backstop: a busy server is
        // waited for, a silent one is an error.
        for &index in &selected {
            let server = state.server_mut(index);
            server.sync_owned()?;
            server.wait_ready()?;
        }

        let mut merged = Partition::default();
        for &index in &selected {
            let server = state.server(index);
            let infos = server.workspace_symbol(&symbol)?;
            merged.absorb(partition(
                &symbol,
                server.name(),
                infos,
                &root,
                scope.as_deref(),
            ));
        }
        let candidates = merged.into_candidates();
        let exclusions = Exclusions {
            out_of_root: candidates.out_of_root,
            out_of_scope: candidates.out_of_scope,
        };
        let exact = match candidates.resolution {
            Resolution::NotFound => {
                return Ok(render_not_found(&root, &symbol, exclusions, &servers_line));
            }
            Resolution::FuzzyOnly(fuzzy) => {
                return Ok(render_fuzzy(
                    &root,
                    &symbol,
                    &fuzzy,
                    exclusions,
                    &servers_line,
                ));
            }
            Resolution::ExactOne(candidate) => vec![*candidate],
            Resolution::ExactMany(exact) => exact,
        };

        // Nomination is not identity: ask each exact candidate's server where
        // it actually goes, then group by destination.
        let mut found = Vec::with_capacity(exact.len());
        for candidate in exact {
            let set = definition_of(state, &root, &symbol, &candidate)?;
            found.push((candidate, set));
        }
        let mut identities = group_by_target(found);
        if identities.len() != 1 {
            return Ok(render_ambiguous(
                &root,
                &symbol,
                &identities,
                exclusions,
                &servers_line,
            ));
        }
        let identity = identities.pop().expect("exactly one");
        let Some(target) = identity.target.clone() else {
            return Ok(render_null_definition(
                &root,
                &symbol,
                &identity,
                &servers_line,
            ));
        };

        let index = state
            .index_of(&identity.server)
            .with_context(|| format!("{} is no longer running", identity.server))?;
        let server = state.server_mut(index);
        let encoding = server.encoding();

        // Own the target's file and check its range the same way the
        // candidates were checked.
        let raw_path = candidate_path_of(&target)?;
        let target_path = std::fs::canonicalize(&raw_path).unwrap_or(raw_path);
        let target_text = server.sync(&target_path)?.text;
        verify_name_at(&target_text, target.selection_range, &symbol, encoding).with_context(
            || {
                format!(
                    "{}'s definition target for '{symbol}' ({}) does not match the file on \
                     disk; the server's view was stale — retry after it re-indexes",
                    identity.server,
                    selection_line(&root, &target.uri, target.selection_range.start)
                )
            },
        )?;
        let target_offset =
            position_to_offset(&target_text, target.selection_range.start, encoding)
                .context("definition target range is not inside the target file as read from disk")?;
        let language = server.language_id(&target_path);
        let (body, body_provenance) = shape_body(&target_text, target_offset, &target, &language, encoding)?;

        let origin = &identity.candidates[0];
        let mut text = String::new();
        text.push_str(&format!(
            "Resolved: '{symbol}' — {} · {} · {}\n",
            origin.info.name,
            selection_line(&root, &origin.info.location.uri, origin.info.location.range.start),
            identity.server,
        ));
        if identity.candidates.len() > 1 {
            text.push_str(&format!(
                "{} symbol entries navigate to this one definition.\n",
                identity.candidates.len()
            ));
        }
        let provenance_line = match target.provenance {
            NavigationProvenance::LocationRange => "navigation point from Location.range",
            NavigationProvenance::LinkSelectionRange => {
                "navigation point from LocationLink.targetSelectionRange"
            }
            NavigationProvenance::LinkTargetRange => {
                "navigation point fell back to LocationLink.targetRange (no targetSelectionRange)"
            }
        };
        text.push_str(&format!(
            "Definition: {} ({provenance_line})\n",
            selection_line(&root, &target.uri, target.selection_range.start),
        ));
        if !target_path.starts_with(&root) {
            text.push_str(&format!(
                "Definition target is outside the workspace root ({}): dependency \
                 destinations are shown read-only.\n",
                root.display()
            ));
        }
        text.push('\n');
        text.push_str(&body);
        text.push('\n');
        text.push_str(&format!("body: {body_provenance}\n"));
        text.push_str(&exclusion_note(exclusions));
        text.push_str(&footer(&root, &servers_line));
        Ok(text)
    }
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
    let index = state
        .index_of(&candidate.server)
        .with_context(|| format!("{} is no longer running", candidate.server))?;
    let server: &mut LspServer = state.server_mut(index);
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
            selection_line(root, &candidate.info.location.uri, range.start)
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

/// The decorated item containing the navigation point, for languages with a
/// grammar (Rust only in this slice); otherwise, or when the shape is
/// unresolved, the server's own range explicitly — never a guessed item.
fn shape_body(
    target_text: &str,
    target_offset: usize,
    target: &DefinitionTarget,
    language: &str,
    encoding: Encoding,
) -> Result<(String, String)> {
    let shaped = if language == "rust" {
        shape::item_at(target_text, target_offset)?
    } else {
        None
    };
    Ok(match shaped {
        Some(item) => {
            let header = match &item.name {
                Some(name) => format!("{} {name}", item.display_kind),
                None => item.header(target_text),
            };
            (
                budget(item.decorated_text(target_text)),
                format!("item body from tree-sitter shaping ({header})"),
            )
        }
        None => {
            let fallback = fallback_body(target_text, target, encoding).unwrap_or_else(|| {
                "<no body could be extracted: the server's range did not resolve inside the \
                 target file; nothing was guessed>"
                    .to_string()
            });
            let why = if language == "rust" {
                "shaping unavailable for this position".to_string()
            } else {
                format!("no shaping grammar for {language} in this slice")
            };
            (
                fallback,
                format!("{why} — server range shown as-is, no item was guessed"),
            )
        }
    })
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
            "qualified paths (`{trimmed}`) are not supported in this slice: only bare symbol \
             names are accepted and matched exactly. Pass the bare name — e.g. `new` for \
             `Config::new`. This restriction is stated rather than silently relaxed."
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
            "scope path `{user_path}` escapes the workspace root ({}); the root is fixed at \
             server start and `path` is only a scope inside it",
            root.display()
        );
    }
    Ok(canonical)
}

#[cfg(test)]
mod tests;
