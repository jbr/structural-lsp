//! Rendering for `show`: every string the tool returns is built here.

use super::*;
use crate::lsp::session::Session;

pub(super) fn render_not_found(
    root: &Path,
    symbol: &str,
    out_of_root: usize,
    out_of_scope: usize,
    server_line: &str,
) -> String {
    format!(
        "No symbol named '{symbol}' was reported by workspace/symbol.\n\
         This is the server's fuzzy index queried with the bare name; the result is not \
         proven exhaustive.\nNo definition was queried.\n{exclusions}{footer}",
        exclusions = exclusion_note(out_of_root, out_of_scope),
        footer = footer(root, server_line),
    )
}

pub(super) fn render_fuzzy(
    root: &Path,
    symbol: &str,
    fuzzy: &[Candidate],
    out_of_root: usize,
    out_of_scope: usize,
    server_line: &str,
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
    text.push_str(&footer(root, server_line));
    text
}

pub(super) fn render_ambiguous(
    root: &Path,
    symbol: &str,
    exact: &[Candidate],
    out_of_root: usize,
    out_of_scope: usize,
    server_line: &str,
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
    text.push_str(&footer(root, server_line));
    text
}

/// More than one definition destination: all are listed (bounded), none is
/// picked, and no body is rendered — an arbitrary first pick would present a
/// choice the caller never made.
pub(super) fn render_multiple_definitions(
    root: &Path,
    symbol: &str,
    discovery: &Discovery,
    targets: &[DefinitionTarget],
    server_line: &str,
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
    text.push_str(&footer(root, server_line));
    text
}

pub(super) fn render_null_definition(
    root: &Path,
    symbol: &str,
    discovery: &Discovery,
    server_line: &str,
) -> String {
    format!(
        "The server reports no definition for '{symbol}' at {} (null/empty \
         textDocument/definition result).\n{footer}",
        selection_line(
            root,
            &discovery.candidate.info.location.uri,
            discovery.candidate.info.location.range.start
        ),
        footer = footer(root, server_line),
    )
}

/// The server's name and version, as reported at `initialize`.
pub(super) fn server_line(session: &Session) -> String {
    session
        .server_info()
        .and_then(|info| {
            let name = info.get("name").and_then(Value::as_str)?;
            let version = info.get("version").and_then(Value::as_str).unwrap_or("?");
            Some(format!("{name} {version}"))
        })
        .unwrap_or_else(|| "rust-analyzer (serverInfo not reported)".to_string())
}

/// Candidate lines, capped, for the fuzzy and ambiguous listings.
pub(super) fn listed_candidates(root: &Path, infos: &[&lsp_types::SymbolInformation]) -> String {
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
pub(super) fn exclusion_note(out_of_root: usize, out_of_scope: usize) -> String {
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
pub(super) fn fallback_body(target_text: &str, target: &DefinitionTarget) -> Option<String> {
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

/// The honest, concise footer: the fixed root, the one view, the server. The
/// schema has no `view` field; the default behavior (definition) is
/// described, and nothing unimplemented is advertised.
pub(super) fn footer(root: &Path, server_line: &str) -> String {
    format!(
        "\nworkspace root (fixed at server start): {}\nview: definition by default (the only \
         view in this slice; bare-name resolution only — no line:col or anchor input).\n\
         server: {server_line}\n",
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
