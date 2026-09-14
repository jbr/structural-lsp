//! Rendering for `show`: every string the tool returns is built here.
//!
//! Every byte is paid for on every call. The happy path is one header line
//! and the body; notes appear only when something is degraded, one line
//! each; listings are one line per entry. The caveats (fuzzy index, fixed
//! root, bare names) live in the tool description and the server's
//! instructions, once, not in every result.

use super::*;
use crate::lsp::definition::Candidate;
use crate::lsp::positions::Encoding;
use lsp_types::SymbolKind;

/// The happy path: `kind name · path:line`, the body, then any notes.
pub(super) fn render_resolved(
    root: &Path,
    kind_label: &str,
    name: &str,
    location: &lsp_types::Location,
    body: &str,
    notes: &[String],
) -> String {
    let mut text = format!(
        "{kind_label} {name} · {}\n{body}\n",
        location_line(root, &location.uri, location.range.start)
    );
    for note in notes {
        text.push_str(&format!("note: {note}\n"));
    }
    text
}

pub(super) fn render_not_found(symbol: &str, exclusions: Exclusions) -> String {
    format!("no symbol named '{symbol}'\n{}", exclusion_notes(exclusions))
}

/// Fuzzy hits only: listed, never resolved. No definition was queried, so
/// there is no signature to give.
pub(super) fn render_fuzzy(
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

/// One line per distinct definition, with its signature when the server's
/// outline gave one. Nothing is picked. The server is named only when the
/// definitions span more than one.
pub(super) fn render_ambiguous(
    root: &Path,
    symbol: &str,
    entries: &[AmbiguousEntry],
    exclusions: Exclusions,
) -> String {
    let servers: std::collections::BTreeSet<&str> =
        entries.iter().map(|entry| entry.server.as_str()).collect();
    let mut text = format!("ambiguous: {} definitions of '{symbol}'\n", entries.len());
    for entry in entries.iter().take(MAX_LISTED) {
        text.push_str(&format!(
            "{} {} · {}",
            entry.kind_label,
            entry.name,
            location_line(root, &entry.location.uri, entry.location.range.start)
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
pub(super) struct AmbiguousEntry {
    pub kind_label: &'static str,
    pub name: String,
    pub server: String,
    /// The definition target, or the symbol's own location when the server
    /// reported no definition.
    pub location: lsp_types::Location,
    pub signature: Option<String>,
}

pub(super) fn render_null_definition(
    root: &Path,
    symbol: &str,
    server: &str,
    location: &lsp_types::Location,
) -> String {
    format!(
        "no definition reported for '{symbol}' ({server}, from {})\n",
        location_line(root, &location.uri, location.range.start)
    )
}

/// `path:line`, 1-based, relative to the root when inside it.
pub(super) fn location_line(root: &Path, uri: &lsp_types::Uri, position: lsp_types::Position) -> String {
    format!("{}:{}", relative_path(root, uri), position.line + 1)
}

pub(super) fn relative_path(root: &Path, uri: &lsp_types::Uri) -> String {
    crate::lsp::definition::candidate_path(uri)
        .map(|path| {
            path.strip_prefix(root)
                .map(|relative| relative.display().to_string())
                .unwrap_or_else(|_| path.display().to_string())
        })
        .unwrap_or_else(|| uri.to_string())
}

/// What the filters excluded, when they excluded anything: the answer may
/// be there.
pub(super) fn exclusion_notes(exclusions: Exclusions) -> String {
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

/// The raw body from the server's own ranges when shaping has no answer: the
/// full range when the server gave one, else the selection range — chosen as
/// a whole. An endpoint of one range is never mixed with an endpoint of the
/// other (a half-shifted pair would render an invented span); an unusable
/// range yields nothing.
pub(super) fn fallback_body(
    target_text: &str,
    target: &DefinitionTarget,
    encoding: Encoding,
) -> Option<String> {
    let (start, end) = match target.full_range {
        Some(range) => (
            position_to_offset(target_text, range.start, encoding).ok()?,
            position_to_offset(target_text, range.end, encoding).ok()?,
        ),
        None => (
            position_to_offset(target_text, target.selection_range.start, encoding).ok()?,
            position_to_offset(target_text, target.selection_range.end, encoding).ok()?,
        ),
    };
    if end > start {
        Some(target_text[start..end].to_string())
    } else {
        None
    }
}

/// Trim a body to the budget on a character boundary. The note says what was
/// cut; the body itself carries no marker.
pub(super) fn budget(body: &str) -> (String, Option<String>) {
    if body.len() <= BODY_BUDGET_BYTES {
        return (body.to_string(), None);
    }
    let mut end = BODY_BUDGET_BYTES;
    while !body.is_char_boundary(end) {
        end -= 1;
    }
    (
        body[..end].to_string(),
        Some(format!(
            "truncated after {end} of {} bytes (result budget)",
            body.len()
        )),
    )
}
