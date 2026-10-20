//! The resolution pipeline every tool shares: a target (a name, or an
//! anchor in a file) to one *identity*, or to a listing that says why
//! there is not one.
//!
//! Identity, not names: candidates only *nominate*. A bare name, or a
//! `path` naming a directory, is nominated from the servers'
//! `workspace/symbol`; a `path` naming a file is nominated from that file's
//! own outline ([`file_scope`]) instead, so its candidates are the file's
//! own symbols rather than a sample of a bounded cross-workspace response.
//! Every exact
//! candidate is sent to `textDocument/definition`, and the results are
//! deduplicated by target — hits that navigate to the same place are one
//! thing; hits that navigate to different places are listed as ambiguous,
//! with their kinds, paths and signatures, and nothing is picked. When the
//! server also answers `textDocument/declaration`, agreement between the
//! two keeps the answer at a candidate that is the definition, instead of
//! following the hop to the declaration's bare signature (the
//! `definition_site` rule, below).
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

use crate::discovery::evidence::Sources;
use crate::lsp::definition::{
    Candidate, DefinitionSet, DefinitionTarget, Identity, NavigationProvenance, Origin,
    group_by_target, locate_name_at, parse_definition_result, verify_name_at,
};
use crate::lsp::outline::{self, Extent};
use crate::lsp::positions::position_to_offset;
use crate::lsp::server::LspServer;
use crate::tools::listing::{
    AmbiguousEntry, directory_scope_note, location_line, no_index_note, qualifier_scope_note,
    render_ambiguous, render_fuzzy, render_not_found, render_null_definition, render_unqualified,
    unanswered_note,
};
use crate::tools::resolution::nominate::{Nominated, nominate};
use crate::workspace::Workspace;
use anyhow::{Context, Result, bail};
use lsp_types::{Position, SymbolKind};
use serde_json::Value;
use std::path::{Path, PathBuf};

pub mod anchored;
pub mod file_scope;
pub mod nominate;
pub mod qualify;

/// What the filters excluded, carried to the listings.
#[derive(Clone, Copy, Debug, Default)]
pub struct Exclusions {
    pub out_of_root: usize,
    pub out_of_scope: usize,
}

/// What a tool was asked to resolve, as the user gave it, plus the view
/// it is relative to.
#[derive(Debug, Clone, Copy)]
pub struct Target<'a> {
    /// The caller's working directory: what `path` joins onto, what
    /// answers are filtered against, and what locations are displayed
    /// relative to. Not the workspace key, which is the project root
    /// above it and is only what servers are shared under
    /// (`docs/discovery.md`).
    pub view: &'a Path,
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

/// Resolve `target` under the caller's view. Argument errors are errors; every
/// other outcome is a [`Resolved`].
pub fn resolve_target(state: &mut Workspace, target: Target<'_>) -> Result<Resolved> {
    let root = target.view.to_path_buf();
    let scope = target
        .path
        .map(|user_path| resolve_scope(&root, user_path))
        .transpose()?;
    // The query boundary: every filesystem change the watcher
    // delivered before this query was admitted is told to the
    // servers that would answer it, before any index is read
    // (`docs/correctness.md`).
    state.flush_watched(scope.as_deref())?;
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
    state: &mut Workspace,
    root: &Path,
    symbol: &str,
    scope: Option<PathBuf>,
) -> Result<Resolved> {
    let qualified = qualify::parse(symbol)?;
    let display = qualified.display();
    let selected = state.select(scope.as_deref())?;

    let Nominated {
        exact,
        fuzzy,
        exclusions,
        unindexed,
    } = nominate(state, &selected, &qualified.name, root, scope.as_deref())?;
    let (exact, unqualified) = qualify::filter(exact, &qualified);
    if exact.is_empty() {
        let labels = |server: &str, kind: SymbolKind| kind_label(state, server, kind);
        let mut listed = if !unqualified.is_empty() {
            render_unqualified(root, &qualified, &unqualified, &labels)
        } else if fuzzy.is_empty() {
            render_not_found(&display, exclusions)
        } else {
            render_fuzzy(root, &display, &fuzzy, &labels, exclusions)
        };
        // Why the index had nothing comes before what a container could
        // have narrowed: a language with no index never offered one.
        listed.push_str(&no_index_note(
            &unindexed,
            scope.as_deref().is_some_and(Path::is_file),
        ));
        // A server that was asked and has answered nothing all session is a
        // project not set up for it, not an absent name. Fuzzy or
        // container-narrowable hits are candidates, so the note is for the
        // miss alone, as with the directory note below.
        if fuzzy.is_empty() && unqualified.is_empty() {
            listed.push_str(&unanswered_note_for(state, &selected));
        }
        // A directory scope that found nothing at all may sit over files no
        // running server serves; that is routing, not absence, and it gets

        // its own note. Fuzzy or container-narrowable hits are candidates,
        // so the note is for the miss alone.
        if fuzzy.is_empty()
            && unqualified.is_empty()
            && let Some(dir) = scope.as_deref().filter(|scope| scope.is_dir())
        {
            listed.push_str(&directory_languages_note(state, root, dir));
        }
        listed.push_str(qualifier_scope_note(&qualified, scope.is_some()));
        return Ok(Resolved::Listed(listed));
    }

    // Nomination is not identity: ask each exact candidate's server where
    // it actually goes, then group by destination.
    let mut found = Vec::with_capacity(exact.len());
    for candidate in exact {
        let set = definition_of(state, root, &qualified.name, &candidate)?;
        found.push((candidate, set));
    }
    finish(
        state,
        root,
        group_by_target(found),
        &qualified.name,
        &display,
        exclusions,
    )
}

/// Which extensions live under a directory scope that found nothing, and
/// which of them no running server serves.
///
/// A directory scope asks every running server by design
/// (`Workspace::select`), so a miss over an unserved tree reads as an
/// absent name. The scan is the discovery budget walk (`Sources::scan`):
/// presence claims hold under truncation, and build and hidden directories
/// are skipped. Served-ness is what the query observed — running servers —
/// rather than what is configured: a configured server that is down serves
/// nothing either, and the note names running servers so the two cases read
/// the same way (`backlog/directory-scope-message.md`).
fn directory_languages_note(state: &Workspace, root: &Path, dir: &Path) -> String {
    let sources = Sources::scan(dir);
    let contained: Vec<String> = sources.by_extension.keys().cloned().collect();
    if contained.is_empty() {
        return String::new();
    }
    let running: Vec<&LspServer> = state.running().collect();
    let serves = |extension: &str| {
        running.iter().any(|server| {
            server
                .adapter()
                .languages()
                .iter()
                .flat_map(|language| language.extensions.iter())
                .any(|known| known.eq_ignore_ascii_case(extension))
        })
    };
    let unserved: Vec<String> = contained
        .iter()
        .filter(|extension| !serves(extension))
        .cloned()
        .collect();
    let display = dir
        .strip_prefix(root)
        .map(|relative| relative.display().to_string())
        .ok()
        .filter(|relative| !relative.is_empty())
        .unwrap_or_else(|| dir.display().to_string());
    let names = running
        .iter()
        .map(|server| server.name())
        .collect::<Vec<_>>()
        .join(", ");
    directory_scope_note(&format!("{display}/"), &contained, &unserved, &names)
}

/// The asked servers behind a total miss that advertised an index yet have
/// answered nothing all session.
///
/// Only servers that were asked and could have answered: one with no
/// workspace index is unserved rather than silent, and a server that yielded
/// once — from its index or from an outline — has shown it is working
/// (docs/resolution.md).
fn unanswered_note_for(state: &Workspace, selected: &[usize]) -> String {
    let mut servers = Vec::new();
    let mut languages = Vec::new();
    for &index in selected {
        let Ok(server) = state.server(index) else {
            continue;
        };
        if !server.supports("workspaceSymbolProvider") || server.has_answered() {
            continue;
        }
        servers.push(server.name().to_string());
        for language in server.adapter().languages() {
            if !languages.contains(&language.id) {
                languages.push(language.id);
            }
        }
    }
    unanswered_note(&servers, &languages)
}

/// One identity is the resolution; several are the ambiguous listing; one
/// with no definition is stated.
fn finish(
    state: &mut Workspace,
    root: &Path,
    mut identities: Vec<Identity>,
    name: &str,
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
        // The name being resolved, not the one the server advertised for
        // it. A server may spell the container into the name — gopls says
        // `Config.Doubled` — and this name is checked against the file's
        // own text and is what a rename would replace, where only the
        // identifier is present (docs/resolution.md).
        Some(target) => Ok(Resolved::One(Box::new(Found {
            name: name.to_string(),
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

/// Own the candidate's file, check the index's range against the text just
/// read, wait for the server, and ask where the candidate's definition is.
/// A range that no longer names the symbol is a stale index, reported
/// rather than navigated from.
fn definition_of(
    state: &mut Workspace,
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
    // A range wider than the name is not staleness: `location.range` is
    // specified as the range enclosing the symbol, and servers differ on
    // whether they tighten it to the name (docs/correctness.md). The
    // position the definition request needs is the name's, wherever in the
    // range it sits.
    let at = locate_name_at(&synced.text, range, symbol, server.encoding()).with_context(|| {
        format!(
            "{}'s location for '{symbol}' ({}) does not name it in the file on disk",
            candidate.server,
            location_line(root, &candidate.info.location.uri, range.start)
        )
    })?;
    // The didOpen/didChange above precedes this request on the wire, so the
    // server answers against the text we read. Wait for no outstanding work
    // again: the sync may have started some.
    server.wait_ready()?;
    let position = at.position(range);
    let raw = server.definition(&synced.uri, position)?;
    let set = parse_definition_result(raw).with_context(|| {
        format!(
            "{} returned an unrecognizable definition result",
            candidate.server
        )
    })?;
    // A header language answers `definition` with the other site, so the
    // hop lands on a signature with no body. A server that also answers
    // `declaration` distinguishes the sites; see `definition_site`.
    if server.supports("declarationProvider")
        && let Some(stayed) = definition_site(server, &synced.uri, position, candidate, &set)
    {
        return Ok(stayed);
    }
    Ok(set)
}

/// Stay at the candidate when it is the definition and the hop would leave
/// it.
///
/// A header language has two sites for one entity, and `definition` answers
/// each from the other, so answering the hop lands on the declaration — a
/// signature with no body — while the index named the definition. When the
/// server also answers `declaration`, the two requests agree with each
/// other exactly when the candidate is the definition: both name the
/// declaration, from opposite sides. Anything else — disagreement,
/// emptiness, error — is not evidence, and the hop stands
/// (docs/resolution.md).
fn definition_site(
    server: &LspServer,
    uri: &str,
    position: Position,
    candidate: &Candidate,
    set: &DefinitionSet,
) -> Option<DefinitionSet> {
    let declared = server.declaration(uri, position).ok()?;
    let DefinitionSet::One(decl) = parse_definition_result(declared).ok()? else {
        return None;
    };
    let DefinitionSet::One(def) = set else {
        return None;
    };
    if decl.uri != def.uri || decl.selection_range != def.selection_range {
        return None;
    }
    let uri: lsp_types::Uri = serde_json::from_value(Value::String(uri.to_owned())).ok()?;
    let here = DefinitionTarget {
        uri,
        selection_range: candidate.info.location.range,
        full_range: None,
        provenance: NavigationProvenance::LocationRange,
    };
    if decl.uri == here.uri && decl.selection_range == here.selection_range {
        return None;
    }
    Some(DefinitionSet::One(here))
}

/// Own the identity's target file and check that the name sits at the
/// target range; the text read is what every later position converts
/// against. Returns the canonical path and the synced document.
pub fn sync_target(
    state: &mut Workspace,
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
            "{}'s definition target for '{name}' ({}) does not name it in the file on disk",
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
fn ambiguous_entries(state: &mut Workspace, identities: &[Identity]) -> Vec<AmbiguousEntry> {
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
fn signature_of(state: &mut Workspace, server: &str, target: &DefinitionTarget) -> Option<String> {
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

pub fn server_named<'a>(state: &'a mut Workspace, name: &str) -> Result<&'a mut LspServer> {
    let index = state
        .index_of(name)
        .with_context(|| format!("{name} is no longer running"))?;
    state.server_mut(index)
}

pub fn kind_label(state: &Workspace, server: &str, kind: SymbolKind) -> &'static str {
    state
        .index_of(server)
        .and_then(|index| state.server(index).ok())
        .map_or_else(
            || crate::lsp::adapter::default_kind_label(kind),
            |server| server.adapter().kind_label(kind),
        )
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
