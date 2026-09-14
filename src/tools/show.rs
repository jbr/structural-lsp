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

/// Status answer when a bounded settle after a feed expires without
/// quiescence: nothing more is queried and no body is rendered against the
/// unsettled index.
fn render_pending_indexing(root: &Path, session: &mut Session, what: &str) -> String {
    format!(
        "Semantic definitions unavailable: {what} is still being re-indexed; the bounded \
         readiness wait expired without quiescent=true.\n\
         No further query was issued and no body is rendered from a possibly stale index.\n\
         Retry the same call when editing settles.\n{footer}",
        footer = footer(root, &server_line(session), &readiness_evidence(session)),
    )
}

/// Status answer when the bounded wait expires without quiescence: no target
/// was resolved and no definition was queried.
fn pending_status(root: &Path, session: &mut Session) -> String {
    let seen = if session.latest_server_status().is_some() {
        "; a serverStatus was seen, not yet quiescent"
    } else {
        "; no serverStatus yet"
    };
    let pending_line = format!(
        "indexing ({waited:.1}s elapsed; the {bound:.0}s readiness bound was reached without \
         quiescent=true{seen})",
        waited = QUIESCE_BOUND.as_secs_f32(),
        bound = QUIESCE_BOUND.as_secs_f32(),
    );
    format!(
        "Semantic definitions unavailable: rust-analyzer is {pending_line}\n\
         No target was resolved and no definition was queried; this does not mean the symbol \
         is absent.\n\
         Retry the same call when indexing finishes.\n{footer}",
        footer = footer(root, &server_line(session), &pending_line),
    )
}

/// Status answer when source files keep changing across the bounded retry:
/// no body is served, because none could be stated against a stable snapshot.
fn render_unstable(
    root: &Path,
    symbol: &str,
    attempts: usize,
    server_line: &str,
    session: &mut Session,
) -> String {
    format!(
        "No stable result for '{symbol}': the source files kept changing while the server was \
         being refreshed and re-queried ({attempts} bounded attempts).\n\
         Nothing was rendered as current — retry when editing settles.\n{footer}",
        footer = footer(root, server_line, &readiness_evidence(session)),
    )
}

fn render_not_found(
    root: &Path,
    symbol: &str,
    out_of_root: usize,
    out_of_scope: usize,
    server_line: &str,
    session: &mut Session,
) -> String {
    format!(
        "No symbol named '{symbol}' was reported by workspace/symbol.\n\
         This is the server's fuzzy index queried with the bare name; the result is not \
         proven exhaustive.\nNo definition was queried.\n{exclusions}{footer}",
        exclusions = exclusion_note(out_of_root, out_of_scope),
        footer = footer(root, server_line, &readiness_evidence(session)),
    )
}

fn render_fuzzy(
    root: &Path,
    symbol: &str,
    fuzzy: &[crate::lsp::definition::Candidate],
    out_of_root: usize,
    out_of_scope: usize,
    server_line: &str,
    session: &mut Session,
) -> String {
    let mut text = format!(
        "No exact match for '{symbol}'. Possible matches (fuzzy, as reported by \
         workspace/symbol; not proven exhaustive):\n"
    );
    text.push_str(&listed_candidates(
        root,
        &fuzzy
            .iter()
            .map(|candidate| &candidate.info)
            .collect::<Vec<_>>(),
    ));
    text.push_str("Nothing was resolved; no definition was queried.\n");
    text.push_str(&exclusion_note(out_of_root, out_of_scope));
    text.push_str(&footer(root, server_line, &readiness_evidence(session)));
    text
}

fn render_ambiguous(
    root: &Path,
    symbol: &str,
    exact: &[crate::lsp::definition::Candidate],
    out_of_root: usize,
    out_of_scope: usize,
    server_line: &str,
    session: &mut Session,
) -> String {
    let mut text = format!(
        "Ambiguous: {} exact matches for '{symbol}' (as reported by workspace/symbol; not \
         proven exhaustive):\n",
        exact.len()
    );
    text.push_str(&listed_candidates(
        root,
        &exact
            .iter()
            .map(|candidate| &candidate.info)
            .collect::<Vec<_>>(),
    ));
    text.push_str(
        "Nothing was resolved — no candidate was picked for you. Narrow with `path`; \
         qualified-path targeting is not implemented in this slice.\n",
    );
    text.push_str(&exclusion_note(out_of_root, out_of_scope));
    text.push_str(&footer(root, server_line, &readiness_evidence(session)));
    text
}

/// More than one definition destination: all are listed (bounded), none is
/// picked, and no body is rendered — an arbitrary first pick would present a
/// choice the caller never made.
fn render_multiple_definitions(
    root: &Path,
    symbol: &str,
    discovery: &Discovery,
    targets: &[DefinitionTarget],
    server_line: &str,
    session: &mut Session,
) -> String {
    let mut text = format!(
        "The server reports {} definition locations for '{symbol}' (queried at {}):\n",
        targets.len(),
        selection_line(
            root,
            &discovery.candidate.info.location.uri,
            discovery.candidate.info.location.range.start
        )
    );
    for target in targets.iter().take(MAX_LISTED) {
        text.push_str(&format!(
            "  - {} · {}\n",
            selection_line(root, &target.uri, target.selection_range.start),
            match target.provenance {
                NavigationProvenance::LocationRange => "Location.range",
                NavigationProvenance::LinkSelectionRange => "LocationLink.targetSelectionRange",
                NavigationProvenance::LinkTargetRange => "LocationLink.targetRange fallback",
            }
        ));
    }
    if targets.len() > MAX_LISTED {
        text.push_str(&format!("  … {} more\n", targets.len() - MAX_LISTED));
    }
    text.push_str(
        "No single body was rendered — the destinations were not picked for you. Narrow the \
         query (a more specific bare name, or a `path` scope); qualified-path targeting is \
         not implemented in this slice.\n",
    );
    text.push_str(&exclusion_note(
        discovery.out_of_root,
        discovery.out_of_scope,
    ));
    text.push_str(&footer(root, server_line, &readiness_evidence(session)));
    text
}

fn render_null_definition(
    root: &Path,
    symbol: &str,
    discovery: &Discovery,
    server_line: &str,
    session: &mut Session,
) -> String {
    format!(
        "The server reports no definition for '{symbol}' at {} (null/empty \
         textDocument/definition result).\n{footer}",
        selection_line(
            root,
            &discovery.candidate.info.location.uri,
            discovery.candidate.info.location.range.start
        ),
        footer = footer(root, server_line, &readiness_evidence(session)),
    )
}

/// The server's name and version, as reported at `initialize`.
fn server_line(session: &mut Session) -> String {
    session
        .server_info()
        .and_then(|info| {
            let name = info.get("name").and_then(Value::as_str)?;
            let version = info.get("version").and_then(Value::as_str).unwrap_or("?");
            Some(format!("{name} {version}"))
        })
        .unwrap_or_else(|| "rust-analyzer (serverInfo not reported)".to_string())
}

/// Current readiness evidence from the latest observed status: evidence, not
/// a completeness claim.
fn readiness_evidence(session: &mut Session) -> String {
    match session.readiness_current() {
        Readiness::Quiescent { after } => format!(
            "quiescent=true (serverStatus, observed {:.1}s after server start) — latest \
             observed evidence, not a completeness claim",
            after.as_secs_f32()
        ),
        Readiness::Pending => match session.latest_server_status() {
            Some(_) => "latest serverStatus: quiescent=false (supersedes any earlier \
                        quiescence); results may predate re-indexing"
                .to_string(),
            None => "no serverStatus observed yet — readiness unverified".to_string(),
        },
    }
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

/// Candidate lines, capped, for the fuzzy and ambiguous listings.
fn listed_candidates(root: &Path, infos: &[&lsp_types::SymbolInformation]) -> String {
    let mut text = String::new();
    for info in infos.iter().take(MAX_LISTED) {
        text.push_str(&format!(
            "  - {} ({:?}) · {}\n",
            info.name,
            info.kind,
            selection_line(root, &info.location.uri, info.location.range.start)
        ));
    }
    if infos.len() > MAX_LISTED {
        text.push_str(&format!("  … {} more\n", infos.len() - MAX_LISTED));
    }
    text
}

/// `path:line:col`, 1-based, relative to the root when inside it.
fn selection_line(root: &Path, uri: &lsp_types::Uri, position: lsp_types::Position) -> String {
    let path = crate::lsp::definition::candidate_path(uri)
        .map(|path| {
            path.strip_prefix(root)
                .map(|relative| relative.display().to_string())
                .unwrap_or_else(|_| path.display().to_string())
        })
        .unwrap_or_else(|| uri.to_string());
    format!("{}:{}:{}", path, position.line + 1, position.character + 1)
}

/// The target's file path for provenance lines, relative to the root when
/// inside it.
fn target_file_line(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .map(|relative| relative.display().to_string())
        .unwrap_or_else(|_| path.display().to_string())
}

/// What the filters excluded, named rather than silently dropped.
fn exclusion_note(out_of_root: usize, out_of_scope: usize) -> String {
    let mut note = String::new();
    if out_of_root > 0 {
        note.push_str(&format!(
            "{out_of_root} candidate(s) outside the workspace root were excluded (the root is \
             fixed at server start).\n"
        ));
    }
    if out_of_scope > 0 {
        note.push_str(&format!(
            "{out_of_scope} candidate(s) inside the root but outside the requested scope were \
             excluded (not silently dropped).\n"
        ));
    }
    note
}

/// The raw body from the server's own ranges when shaping has no answer: the
/// full range when the server gave one, else the selection range — chosen as
/// a whole. An endpoint of one range is never mixed with an endpoint of the
/// other (a half-shifted pair would render an invented span); an unusable
/// range yields nothing.
fn fallback_body(target_text: &str, target: &DefinitionTarget) -> Option<String> {
    let (start, end) = match target.full_range {
        Some(range) => (
            position_to_offset(target_text, range.start).ok()?,
            position_to_offset(target_text, range.end).ok()?,
        ),
        None => (
            position_to_offset(target_text, target.selection_range.start).ok()?,
            position_to_offset(target_text, target.selection_range.end).ok()?,
        ),
    };
    if end > start {
        Some(budget(&target_text[start..end]))
    } else {
        None
    }
}

/// The honest, concise footer: the fixed root, the one view, the server and
/// readiness evidence. The schema has no `view` field; the default behavior
/// (definition) is described, and nothing unimplemented is advertised.
fn footer(root: &Path, server_line: &str, readiness_line: &str) -> String {
    format!(
        "\nworkspace root (fixed at server start): {}\nview: definition by default (the only \
         view in this slice; bare-name resolution only — no line:col or anchor input).\n\
         server: {server_line} · readiness evidence: {readiness_line}\n",
        root.display()
    )
}

/// Trim a body to the budget, on a character boundary, marking the omission.
fn budget(body: &str) -> String {
    if body.len() <= BODY_BUDGET_BYTES {
        return body.to_string();
    }
    let mut end = BODY_BUDGET_BYTES;
    while !body.is_char_boundary(end) {
        end -= 1;
    }
    format!(
        "{}\n… truncated: showing the first {end} of {} bytes of this item (result budget); \
         pagination is not implemented in this slice",
        &body[..end],
        body.len()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lsp::definition::NavigationProvenance;
    use lsp_types::Uri;

    fn target(
        full_range: Option<lsp_types::Range>,
        selection: lsp_types::Range,
    ) -> DefinitionTarget {
        let uri: Uri = serde_json::from_value(serde_json::json!("file:///ws/src/lib.rs")).unwrap();
        DefinitionTarget {
            uri,
            selection_range: selection,
            full_range,
            provenance: NavigationProvenance::LocationRange,
        }
    }

    fn range(start: (u32, u32), end: (u32, u32)) -> lsp_types::Range {
        lsp_types::Range {
            start: lsp_types::Position {
                line: start.0,
                character: start.1,
            },
            end: lsp_types::Position {
                line: end.0,
                character: end.1,
            },
        }
    }

    const TEXT: &str = "first line\nsecond line\nthird line\n";

    #[test]
    fn fallback_prefers_the_whole_full_range() {
        // full_range spans both lines; the selection range is a strict
        // subset. The body must come from the full range, not the selection.
        let body = fallback_body(
            TEXT,
            &target(Some(range((0, 0), (1, 11))), range((1, 0), (1, 6))),
        )
        .expect("the full range is usable");
        assert_eq!(body, "first line\nsecond line");
    }

    #[test]
    fn fallback_with_an_unusable_full_range_is_none_not_a_mixed_span() {
        // The full range's END points past the file (a stale/invalid range);
        // mixing its start with the selection's end would render an invented
        // span, and mixing with its start would be the old bug. Atomicity:
        // the whole full range is unusable, so nothing is rendered.
        let body = fallback_body(
            TEXT,
            &target(Some(range((0, 0), (99, 0))), range((1, 0), (1, 6))),
        );
        assert_eq!(body, None);
    }

    #[test]
    fn fallback_without_a_full_range_uses_the_selection_range() {
        let body = fallback_body(TEXT, &target(None, range((1, 0), (1, 6))))
            .expect("the selection range is usable");
        assert_eq!(body, "second");
    }

    #[test]
    fn fallback_with_an_unusable_selection_range_is_none() {
        let body = fallback_body(TEXT, &target(None, range((50, 0), (51, 6))));
        assert_eq!(body, None);
    }

    #[test]
    fn fallback_with_an_inverted_range_is_none() {
        let body = fallback_body(TEXT, &target(None, range((1, 6), (1, 0))));
        assert_eq!(body, None, "end before start is not a body");
    }
}

