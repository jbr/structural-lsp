//! The listings every tool can answer with, and the path formatting they
//! share: not found, fuzzy-only, ambiguous, and a hit with no definition.
//!
//! Every byte is paid for on every call: one line per entry, and the
//! caveats (fuzzy index, fixed root, bare names) live in the tool
//! descriptions and the server's instructions, once, not in every result.

use crate::lsp::definition::Candidate;
use crate::tools::resolution::Exclusions;
use crate::tools::resolution::qualify::Qualified;
use lsp_types::{Location, Position, SymbolKind, Uri};
use std::path::Path;

/// Entries listed before "… N more".
pub const MAX_LISTED: usize = 20;

pub fn render_not_found(symbol: &str, exclusions: Exclusions) -> String {
    format!(
        "no symbol named '{symbol}'\n{}",
        exclusion_notes(exclusions)
    )
}

/// Fuzzy hits only: listed, never resolved. No definition was queried, so
/// there is no signature to give.
pub fn render_fuzzy(
    root: &Path,
    symbol: &str,
    fuzzy: &[Candidate],
    labels: &dyn Fn(&str, SymbolKind) -> &'static str,
    exclusions: Exclusions,
) -> String {
    let mut text = format!("no exact match for '{symbol}'; similar:\n");
    for candidate in fuzzy.iter().take(MAX_LISTED) {
        let info = &candidate.info;
        text.push_str(&format!(
            "{} {} · {}\n",
            labels(&candidate.server, info.kind),
            info.name,
            location_line(root, &info.location.uri, info.location.range.start)
        ));
    }
    if fuzzy.len() > MAX_LISTED {
        text.push_str(&format!("… {} more\n", fuzzy.len() - MAX_LISTED));
    }
    text.push_str(&exclusion_notes(exclusions));
    text
}

/// `kind name · path:line`, or `name · path:line` when no kind is known or
/// the name already starts with the kind word (rust-analyzer names an impl
/// block `impl Config`).
pub fn header(kind_label: Option<&str>, name: &str, location: &str) -> String {
    match kind_label {
        Some(kind) if !name.starts_with(&format!("{kind} ")) => {
            format!("{kind} {name} · {location}")
        }
        _ => format!("{name} · {location}"),
    }
}

/// What to say when a qualified name found nothing in the index, and no
/// file scope was in effect.
///
/// `workspace/symbol` carries one optional `containerName`, and a server is
/// conforming whether it names the class, names the module, or omits it:
/// ty reports none, jedi indexes no class members at all, and pylsp has no
/// index. `documentSymbol` nests instead, so a `path` naming the declaring
/// file resolves the same qualifier from the file's own outline — every
/// server the conformance probe asks answers it that way
/// (`docs/resolution.md`). Empty when the caller already gave a scope, or
/// when the name carried no qualifier and there is nothing to relocate.
pub fn qualifier_scope_note(qualified: &Qualified, scoped: bool) -> &'static str {
    if qualified.qualifiers.is_empty() || scoped {
        ""
    } else {
        "note: the symbol index carries at most one container per symbol, and some servers report none; naming the declaring file in `path` resolves the qualifier from that file's outline instead\n"
    }
}

/// What to say when a running server has no workspace symbol index at all,
/// and the query was a name rather than a file.
///
/// `workspaceSymbolProvider` is optional, and three of the servers the
/// conformance probe asks — pylsp, gleam and qmlls — serve documents and no
/// index (`reference/lsp-server-conventions.md`). A bare name is
/// nominated from the index, so for those languages it cannot reach a
/// symbol that is there, and without this the miss is spelled the same way
/// as a symbol that is not.
///
/// The languages are named and the servers are not: which executable serves
/// a language is not the caller's problem, and the actionable part is the
/// language and the `path` that routes around it. Empty when the caller
/// already named a file, which is the thing this would ask for.
pub fn no_index_note(unindexed: &[String], file_scoped: bool) -> String {
    if unindexed.is_empty() || file_scoped {
        return String::new();
    }
    format!(
        "note: no symbol index is available for {}; a name defined there resolves only when \
         `path` names its file\n",
        unindexed.join(", ")
    )
}

/// A directory scope that found nothing, over files no running server serves.
///
/// A file scope with no server refuses outright (`Workspace::select`), but a
/// directory scope asks every running server by design, so a tree in an
/// unserved language misses as an absent name. When the directory holds
/// extensions no running server's languages cover, the miss says which
/// extensions and where, with the running servers named. Empty when every
/// extension under the scope is served (docs/resolution.md).
pub fn directory_scope_note(
    scope: &str,
    contained: &[String],
    unserved: &[String],
    running: &str,
) -> String {
    if unserved.is_empty() {
        return String::new();
    }
    format!(
        "note: `{scope}` holds {} files; no running language server serves {} files (running: {running})\n",
        join_capped(contained, 8),
        join_capped(unserved, 5),
    )
}

/// Servers whose census stopped at the walk's file budget, on a miss.
///
/// The query boundary pushes every served file written since the last
/// census, and the census stops where the evidence scan does, so in a tree
/// past the budget an edit beyond it reaches the server only by its own
/// watching — a miss there may be a symbol not seen yet rather than one
/// that is not there (docs/correctness.md). Empty when every census
/// covered its tree.
pub fn unreconciled_note(servers: &[String]) -> String {
    if servers.is_empty() {
        return String::new();
    }
    format!(
        "note: this tree has more than {} files, so {} may not yet have seen a recent edit to a file past that budget; naming its file in `path` syncs it\n",
        crate::discovery::evidence::FILE_BUDGET,
        servers.join(", "),
    )
}

/// Servers that advertised an index, were asked, and have answered nothing
/// all session.
///
/// An empty answer from such a server is a different claim from an empty
/// answer from a working one: the server started but may not be configured
/// for this project. A server with no index to ask is not silent, it is
/// unindexed, and gets [`no_index_note`] instead. Empty when every asked
/// server has answered something (docs/resolution.md).
pub fn unanswered_note(servers: &[String], languages: &[String]) -> String {
    if servers.is_empty() {
        return String::new();
    }
    let verb = if servers.len() == 1 { "has" } else { "have" };
    format!(
        "note: {} started but {} answered nothing this session; the project may not be set up for {} files\n",
        servers.join(", "),
        verb,
        join_capped(languages, 5),
    )
}

/// Backticked extensions, capped so one wild directory cannot flood a miss.
fn join_capped(extensions: &[String], limit: usize) -> String {
    let mut items: Vec<String> = extensions
        .iter()
        .take(limit)
        .map(|extension| format!("`{extension}`"))
        .collect();
    if extensions.len() > limit {
        items.push(format!("and {} more", extensions.len() - limit));
    }
    items.join(", ")
}

/// Exact hits by name, none in a container matching the qualifier: listed
/// with the containers the server reported, so the qualifier can be fixed.
pub fn render_unqualified(
    root: &Path,
    qualified: &Qualified,
    candidates: &[Candidate],
    labels: &dyn Fn(&str, SymbolKind) -> &'static str,
) -> String {
    let mut text = format!(
        "no '{}' is in a container ending in `{}`; the '{}' candidates:\n",
        qualified.name,
        qualified.qualifiers.join("::"),
        qualified.name
    );
    for candidate in candidates.iter().take(MAX_LISTED) {
        let info = &candidate.info;
        text.push_str(&format!(
            "{} {} · {} · {}\n",
            labels(&candidate.server, info.kind),
            info.name,
            location_line(root, &info.location.uri, info.location.range.start),
            match info.container_name.as_deref() {
                Some(container) => format!("in {container}"),
                None => "no container".to_string(),
            }
        ));
    }
    if candidates.len() > MAX_LISTED {
        text.push_str(&format!("… {} more\n", candidates.len() - MAX_LISTED));
    }
    text
}

/// One line per distinct definition, with its signature when the server's
/// outline gave one. Nothing is picked. The server is named only when the
/// definitions span more than one.
pub fn render_ambiguous(
    root: &Path,
    symbol: &str,
    entries: &[AmbiguousEntry],
    exclusions: Exclusions,
) -> String {
    let servers: std::collections::BTreeSet<&str> =
        entries.iter().map(|entry| entry.server.as_str()).collect();
    let mut text = format!("ambiguous: {} definitions of '{symbol}'\n", entries.len());
    for entry in entries.iter().take(MAX_LISTED) {
        text.push_str(&header(
            entry.kind_label,
            &entry.name,
            &location_line(root, &entry.location.uri, entry.location.range.start),
        ));
        if let Some(signature) = &entry.signature {
            text.push_str(&format!(" · {signature}"));
        }
        if servers.len() > 1 {
            text.push_str(&format!(" · {}", entry.server));
        }
        text.push('\n');
    }
    if entries.len() > MAX_LISTED {
        text.push_str(&format!("… {} more\n", entries.len() - MAX_LISTED));
    }
    text.push_str(&exclusion_notes(exclusions));
    text
}

/// One entry of the ambiguous listing, already resolved to display data.
pub struct AmbiguousEntry {
    /// `None` for an anchored origin whose kind no outline confirmed.
    pub kind_label: Option<&'static str>,
    pub name: String,
    pub server: String,
    /// The definition target, or the symbol's own location when the server
    /// reported no definition.
    pub location: Location,
    pub signature: Option<String>,
}

pub fn render_null_definition(
    root: &Path,
    symbol: &str,
    server: &str,
    location: &Location,
) -> String {
    format!(
        "no definition reported for '{symbol}' ({server}, from {})\n",
        location_line(root, &location.uri, location.range.start)
    )
}

/// `path:line`, 1-based, relative to the root when inside it.
pub fn location_line(root: &Path, uri: &Uri, position: Position) -> String {
    format!("{}:{}", relative_path(root, uri), position.line + 1)
}

pub fn relative_path(root: &Path, uri: &Uri) -> String {
    crate::lsp::definition::candidate_path(uri)
        .map(|path| relative_display(root, &path))
        .unwrap_or_else(|| uri.to_string())
}

/// `path` relative to `root` when inside it, else as it is.
pub fn relative_display(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .map(|relative| relative.display().to_string())
        .unwrap_or_else(|_| path.display().to_string())
}

/// What the filters excluded, when they excluded anything: the answer may
/// be there.
pub fn exclusion_notes(exclusions: Exclusions) -> String {
    let Exclusions {
        out_of_root,
        out_of_scope,
    } = exclusions;
    let mut note = String::new();
    if out_of_root > 0 {
        note.push_str(&format!(
            "note: {out_of_root} candidate(s) outside the workspace root were excluded\n"
        ));
    }
    if out_of_scope > 0 {
        note.push_str(&format!(
            "note: {out_of_scope} candidate(s) outside the `path` scope were excluded\n"
        ));
    }
    note
}
