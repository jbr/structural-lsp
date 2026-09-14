//! `show` — resolve a symbol by bare name and return its definition's
//! decorated source, not just a location.
//!
//! Slice boundary (deliberate): bare exact symbol names only, one fixed
//! workspace root, definition only. No anchor/focus targeting, no line:col
//! input, no qualified paths (rejected with guidance — a bare-name exact
//! match is the only resolution that can be honestly validated), no other
//! views. Results state readiness evidence, separate exact from fuzzy
//! candidates without claiming search completeness, report the resolved
//! selection before the body, and mark omissions.
//!
//! Freshness order (review-mandated): every previously opened document is
//! refreshed from disk *before* discovery; if the chosen candidate's file or
//! the definition target's file changes relative to what the server knew,
//! the affected query is re-run once, bounded; files that keep changing
//! yield a retry/status answer, never a stale body presented as current.

use crate::lsp::definition::{
    DefinitionSet, DefinitionTarget, NavigationProvenance, Resolution, parse_definition_result,
    resolve,
};
use crate::lsp::positions::position_to_offset;
use crate::lsp::session::{Readiness, Session};
use crate::lsp::shape;
use crate::state::{QUERY_TIMEOUT, QUIESCE_BOUND, State};
use anyhow::{Context, Result, bail};
use mcplease::{
    traits::{Tool, ToolMeta},
    types::{RequestContext, ToolAnnotations},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
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
/// outside the root are always excluded, symlink or not). Every result
/// states the resolved selection, readiness evidence, and the fixed root.
#[derive(Serialize, Deserialize, Debug, JsonSchema, clap::Args)]
#[serde(rename = "show")]
#[group(skip)]
pub struct Show {
    /// Exact bare symbol name (e.g. `Config`). Qualified paths
    /// (`Config::new`) are rejected: they cannot be validated by bare-name
    /// matching in this slice.
    pub symbol: String,

    /// Optional scope: a file or directory under the workspace root, relative
    /// to it. Narrows `workspace/symbol` candidates; never changes the
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

/// One discovery→refresh round: the chosen candidate with the filter's
/// exclusion counts, and the refreshed snapshot of its file.
struct Discovery {
    out_of_root: usize,
    out_of_scope: usize,
    candidate: Box<crate::lsp::definition::Candidate>,
    candidate_uri: String,
    candidate_text: String,
}

impl Tool<State> for Show {
    type Output = String;

    fn execute(self, state: &mut State, _context: &RequestContext) -> Result<Self::Output> {
        // Everything rendering needs is captured up front: the session holds
        // the mutable borrow for the rest of the call.
        let root = state.root().to_path_buf();
        let symbol = validate_symbol(&self.symbol)?;
        let scope = self
            .path
            .as_deref()
            .map(|user_path| resolve_scope(&root, user_path))
            .transpose()?;

        let session = state.session()?;
        let readiness = session
            .ensure_ready(QUIESCE_BOUND)
            .context("rust-analyzer stopped responding while waiting for readiness")?;
        if let Readiness::Pending = readiness {
            return Ok(pending_status(&root, session));
        }

        let server_line = server_line(session);

        // Freshness before discovery: re-read every previously opened
        // document and feed the server when its copy is stale, so
        // `workspace/symbol` runs against the server's current view. A fed
        // change may start re-indexing; wait for the epoch to go quiescent
        // again (bounded).
        let fed = session
            .refresh_opened()
            .context("rust-analyzer stopped responding while refreshing opened documents")?;
        if fed > 0 {
            let readiness = session
                .settle(QUIESCE_BOUND)
                .context("rust-analyzer stopped responding while re-indexing changed files")?;
            if let Readiness::Pending = readiness {
                return Ok(render_pending_indexing(
                    &root,
                    session,
                    "the refreshed documents",
                ));
            }
        }

        // Discovery, re-runnable once when the chosen candidate's file turns
        // out to have changed relative to the server's index.
        let mut discovery_attempts = 0usize;
        let discovery = loop {
            let infos = session
                .workspace_symbol(&symbol, QUERY_TIMEOUT)
                .context("rust-analyzer stopped responding during workspace/symbol")?;
            let candidates = resolve(&symbol, infos, &root, scope.as_deref());

            let (out_of_root, out_of_scope) = (candidates.out_of_root, candidates.out_of_scope);
            match candidates.resolution {
                Resolution::NotFound => {
                    return Ok(render_not_found(
                        &root,
                        &symbol,
                        out_of_root,
                        out_of_scope,
                        &server_line,
                        session,
                    ));
                }
                Resolution::FuzzyOnly(fuzzy) => {
                    return Ok(render_fuzzy(
                        &root,
                        &symbol,
                        &fuzzy,
                        out_of_root,
                        out_of_scope,
                        &server_line,
                        session,
                    ));
                }
                Resolution::ExactMany(exact) => {
                    return Ok(render_ambiguous(
                        &root,
                        &symbol,
                        &exact,
                        out_of_root,
                        out_of_scope,
                        &server_line,
                        session,
                    ));
                }
                Resolution::ExactOne(_) => {}
            }
            let Resolution::ExactOne(candidate) = candidates.resolution else {
                unreachable!("matched ExactOne above")
            };

            let candidate_file = candidate
                .canonical_path
                .clone()
                .ok_or_else(|| anyhow::anyhow!("candidate URI is not a readable file path"))?;
            let (candidate_uri, candidate_fed, candidate_text) = session
                .refresh_document(&candidate_file)
                .context("rust-analyzer stopped responding while refreshing the candidate")?;
            if !candidate_fed {
                break Discovery {
                    out_of_root,
                    out_of_scope,
                    candidate,
                    candidate_uri,
                    candidate_text,
                };
            }
            // The file changed relative to the server's index: the symbol
            // ranges discovery just saw may be stale. Re-wait for re-indexing
            // and re-run discovery, once; still-changing files yield a
            // retry/status answer instead of a body.
            discovery_attempts += 1;
            if discovery_attempts > 1 {
                return Ok(render_unstable(
                    &root,
                    &symbol,
                    discovery_attempts,
                    &server_line,
                    session,
                ));
            }
            let readiness = session
                .settle(QUIESCE_BOUND)
                .context("rust-analyzer stopped responding while re-indexing the candidate")?;
            if let Readiness::Pending = readiness {
                return Ok(render_pending_indexing(
                    &root,
                    session,
                    "the re-queried candidate",
                ));
            }
        };

        let navigation = position_to_offset(
            &discovery.candidate_text,
            discovery.candidate.info.location.range.start,
        )
        .context("candidate range is not inside the candidate file as read from disk")?;
        let _ = navigation; // used implicitly: the definition query reuses
        // the candidate's own range start; the offset existed

        // Definition + target synchronization: query at the candidate's own
        // range start; open the target file (feeding the server when its
        // state was unknown or stale) and re-query once, bounded, before
        // shaping against that snapshot.
        let mut target_attempts = 0usize;
        let mut target_was_unknown = false;
        let (target, target_text) = loop {
            let raw = session
                .definition(
                    &discovery.candidate_uri,
                    discovery.candidate.info.location.range.start,
                    QUERY_TIMEOUT,
                )
                .context("rust-analyzer stopped responding during textDocument/definition")?;
            let set = parse_definition_result(raw)
                .context("rust-analyzer returned an unrecognizable definition result")?;
            let target = match set {
                DefinitionSet::None => {
                    return Ok(render_null_definition(
                        &root,
                        &symbol,
                        &discovery,
                        &server_line,
                        session,
                    ));
                }
                DefinitionSet::Many(targets) => {
                    return Ok(render_multiple_definitions(
                        &root,
                        &symbol,
                        &discovery,
                        &targets,
                        &server_line,
                        session,
                    ));
                }
                DefinitionSet::One(target) => target,
            };
            let raw_path = candidate_path_of(&target)?;
            let target_file = std::fs::canonicalize(&raw_path).unwrap_or(raw_path);
            let (_target_uri, target_fed, target_text) = session
                .refresh_document(&target_file)
                .context("rust-analyzer stopped responding while opening the definition target")?;
            if !target_fed {
                break (target, target_text);
            }
            // The target's on-disk state was unknown to the server (first
            // open) or had changed: re-run the bounded query against the now
            // known snapshot, once.
            target_attempts += 1;
            if target_attempts > 1 {
                return Ok(render_unstable(
                    &root,
                    &symbol,
                    target_attempts,
                    &server_line,
                    session,
                ));
            }
            target_was_unknown = true;
            let readiness = session
                .settle(QUIESCE_BOUND)
                .context("rust-analyzer stopped responding while re-indexing the target")?;
            if let Readiness::Pending = readiness {
                return Ok(render_pending_indexing(
                    &root,
                    session,
                    "the re-queried definition target",
                ));
            }
        };

        let target_path = candidate_path_of(&target)?;
        let target_offset = position_to_offset(&target_text, target.selection_range.start)
            .context("definition target range is not inside the target file as read from disk")?;

        // Shaping: the decorated item containing the navigation point; an
        // unresolved shape falls back to the server's own range explicitly,
        // never to a guessed item.
        let (body, body_provenance) = match shape::item_at(&target_text, target_offset)? {
            Some(item) => {
                let header = match &item.name {
                    Some(name) => format!("{} {name}", item.display_kind),
                    None => item.header(&target_text),
                };
                (
                    budget(item.decorated_text(&target_text)),
                    format!("item body from tree-sitter shaping ({header})"),
                )
            }
            None => {
                let fallback = fallback_body(&target_text, &target).unwrap_or_else(|| {
                    "<no body could be extracted: the server's range did \
                                         not resolve inside the target file; nothing was guessed>"
                        .to_string()
                });
                (
                    fallback,
                    "shaping unavailable for this position — server range shown as-is, no item \
                     was guessed"
                        .to_string(),
                )
            }
        };

        let mut text = String::new();
        text.push_str(&format!(
            "Resolved: '{symbol}' — {} · {}\n",
            discovery.candidate.info.name,
            selection_line(
                &root,
                &discovery.candidate.info.location.uri,
                discovery.candidate.info.location.range.start
            )
        ));
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
        if target_was_unknown {
            text.push_str(&format!(
                "The definition was re-queried after opening {} (its on-disk state was unknown \
                 to the server before this call); the shown body reflects that snapshot.\n",
                target_file_line(&root, &target_path),
            ));
        }
        if !target_path.starts_with(&root) {
            text.push_str(&format!(
                "Definition target is outside the workspace root ({}): dependency \
                 destinations are shown read-only.\n",
                root.display()
            ));
        }
        text.push_str(&format!(
            "readiness evidence: {server_line} · {} · position encoding {}\n\n",
            readiness_evidence(session),
            session
                .position_encoding()
                .unwrap_or("utf-16 (LSP default; server did not negotiate)")
        ));
        text.push_str(&body);
        text.push('\n');
        text.push_str(&format!("body: {body_provenance}\n"));
        text.push_str(&exclusion_note(
            discovery.out_of_root,
            discovery.out_of_scope,
        ));
        text.push_str(&footer(&root, &server_line, &readiness_evidence(session)));
        Ok(text)
    }
}

/// The filesystem path of a definition target, for reading and refreshing.
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
/// inside the root. Never changes the server's root.
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
