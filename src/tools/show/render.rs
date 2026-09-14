//! Rendering for `show`: every string the tool returns is built here.

use super::*;
use crate::lsp::positions::Encoding;

pub(super) fn render_not_found(
    root: &Path,
    symbol: &str,
    exclusions: Exclusions,
    servers_line: &str,
) -> String {
    format!(
        "No symbol named '{symbol}' was reported by workspace/symbol.\n\
         This is the server's fuzzy index queried with the bare name; the result is not \
         proven exhaustive.\nNo definition was queried.\n{exclusions}{footer}",
        exclusions = exclusion_note(exclusions),
        footer = footer(root, servers_line),
    )
}

pub(super) fn render_fuzzy(
    root: &Path,
    symbol: &str,
    fuzzy: &[Candidate],
    exclusions: Exclusions,
    servers_line: &str,
) -> String {
    let mut text = format!(
        "No exact match for '{symbol}'. Possible matches (fuzzy, as reported by \
         workspace/symbol; not proven exhaustive):\n"
    );
    text.push_str(&listed_candidates(root, fuzzy));
    text.push_str("Nothing was resolved; no definition was queried.\n");
    text.push_str(&exclusion_note(exclusions));
    text.push_str(&footer(root, servers_line));
    text
}

/// Several distinct definition targets: all are listed (bounded) with their
/// kinds and paths, none is picked, and no body is rendered — an arbitrary
/// first pick would present a choice the caller never made.
pub(super) fn render_ambiguous(
    root: &Path,
    symbol: &str,
    identities: &[Identity],
    exclusions: Exclusions,
    servers_line: &str,
) -> String {
    let mut text = format!(
        "Ambiguous: {} exact matches for '{symbol}' (distinct definitions, as reported by \
         workspace/symbol and textDocument/definition; not proven exhaustive):\n",
        identities.len()
    );
    for identity in identities.iter().take(MAX_LISTED) {
        let origin = &identity.candidates[0];
        let location = match &identity.target {
            Some(target) => selection_line(root, &target.uri, target.selection_range.start),
            None => format!(
                "{} (no definition reported)",
                selection_line(
                    root,
                    &origin.info.location.uri,
                    origin.info.location.range.start
                )
            ),
        };
        text.push_str(&format!(
            "  - {} ({:?}) · {location} · {}\n",
            identity.name(),
            identity.kind(),
            identity.server
        ));
    }
    if identities.len() > MAX_LISTED {
        text.push_str(&format!("  … {} more\n", identities.len() - MAX_LISTED));
    }
    text.push_str(
        "Nothing was resolved — no definition was picked for you. Narrow with `path`; \
         qualified-path targeting is not implemented in this slice.\n",
    );
    text.push_str(&exclusion_note(exclusions));
    text.push_str(&footer(root, servers_line));
    text
}

pub(super) fn render_null_definition(
    root: &Path,
    symbol: &str,
    identity: &Identity,
    servers_line: &str,
) -> String {
    let origin = &identity.candidates[0];
    format!(
        "{} reports no definition for '{symbol}' at {} (null/empty \
         textDocument/definition result).\n{footer}",
        identity.server,
        selection_line(
            root,
            &origin.info.location.uri,
            origin.info.location.range.start
        ),
        footer = footer(root, servers_line),
    )
}

/// Candidate lines, capped, for the fuzzy and ambiguous listings: name,
/// kind, location, and which server reported it.
pub(super) fn listed_candidates(root: &Path, candidates: &[Candidate]) -> String {
    let mut text = String::new();
    for candidate in candidates.iter().take(MAX_LISTED) {
        let info = &candidate.info;
        text.push_str(&format!(
            "  - {} ({:?}) · {} · {}\n",
            info.name,
            info.kind,
            selection_line(root, &info.location.uri, info.location.range.start),
            candidate.server
        ));
    }
    if candidates.len() > MAX_LISTED {
        text.push_str(&format!("  … {} more\n", candidates.len() - MAX_LISTED));
    }
    text
}

/// `path:line:col`, 1-based, relative to the root when inside it.
pub(super) fn selection_line(
    root: &Path,
    uri: &lsp_types::Uri,
    position: lsp_types::Position,
) -> String {
    let path = crate::lsp::definition::candidate_path(uri)
        .map(|path| {
            path.strip_prefix(root)
                .map(|relative| relative.display().to_string())
                .unwrap_or_else(|_| path.display().to_string())
        })
        .unwrap_or_else(|| uri.to_string());
    format!("{}:{}:{}", path, position.line + 1, position.character + 1)
}

/// What the filters excluded, named rather than silently dropped.
pub(super) fn exclusion_note(exclusions: Exclusions) -> String {
    let Exclusions {
        out_of_root,
        out_of_scope,
    } = exclusions;
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
        Some(budget(&target_text[start..end]))
    } else {
        None
    }
}

/// The honest, concise footer: the fixed root, the one view, the servers.
/// The schema has no `view` field; the default behavior (definition) is
/// described, and nothing unimplemented is advertised.
pub(super) fn footer(root: &Path, servers_line: &str) -> String {
    format!(
        "\nworkspace root (fixed at server start): {}\nview: definition by default (the only \
         view in this slice; bare-name resolution only — no line:col or anchor input).\n\
         servers: {servers_line}\n",
        root.display()
    )
}

/// Trim a body to the budget, on a character boundary, marking the omission.
pub(super) fn budget(body: &str) -> String {
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
