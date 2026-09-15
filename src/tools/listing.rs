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
