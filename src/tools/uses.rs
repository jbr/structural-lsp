//! `uses` — references to a symbol resolved by bare name, one line of
//! context per hit, grouped by file and enclosing item, tests set apart.
//!
//! Resolution is the shared pipeline in [`crate::tools::resolution`], so a
//! name that `show` would list (not found, fuzzy, ambiguous) is listed here
//! the same way. With one identity: own its target file, check the name
//! sits at the target, wait, `textDocument/references` there without the
//! declaration. Every file with hits is then owned too, and each hit's
//! range is checked against the text just read — it must hold the name or
//! one of the adapter's reference aliases (`Self`), or the server's view of
//! that file was stale and the call is an error, never a listing built on
//! it. The enclosing item comes from the file's `documentSymbol`.

use crate::lsp::outline;
use crate::lsp::positions::{Encoding, position_to_offset};
use crate::lsp::references::{FileHits, group_by_file, parse_references_result, verify_reference_at};
use crate::lsp::server::LspServer;
use crate::state::State;
use crate::tools::listing::{header, location_line, relative_display};
use crate::tools::resolution::{
    Found, Resolved, Target, describe, resolve_target, server_named, sync_target,
};
use anyhow::{Context, Result};
use lsp_types::Position;
use mcplease::{
    traits::{Tool, ToolMeta},
    types::{RequestContext, ToolAnnotations},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::Path;

mod classify;
mod render;
use render::{FileGroup, HitLine, render_uses};

/// Hits listed before the rest is reported as a count.
const MAX_HITS: usize = 100;

/// Find the references to a code symbol: every use the language server
/// knows of, with the source line and the enclosing item for each, grouped
/// by file. Target it as `show` does: by name (`symbol`, bare or qualified,
/// with an optional `path` scope) or by quoting source text (`anchor` in
/// `path`, `focus` naming the token), which reaches locals and parameters.
/// Tests are listed separately by a heuristic: a file or directory named
/// for tests, or a module named `tests`; test code elsewhere (an inline
/// `#[cfg(test)]` block under another name) is listed as production.
///
/// An ambiguous or unknown target returns the same listing `show` gives.
/// The declaration itself is not a reference. References follow symbol
/// identity, not text, so renames, re-exports and method calls are found
/// and same-named strangers are not. The answer is current: owned files
/// are re-synced from disk and the server is waited for before every
/// query. `path` scopes where the *symbol* is looked up, not where its
/// references are listed.
#[derive(Serialize, Deserialize, Debug, JsonSchema, clap::Args)]
#[serde(rename = "uses")]
#[group(skip)]
pub struct Uses {
    /// The symbol's name: bare (`choose`) or qualified by its container
    /// (`Config::new`, `Config.doubled`). Exact match on the name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,

    /// A file or directory under the workspace root, relative to it: a
    /// scope for `symbol`, or the file an `anchor` is quoted from. A file
    /// also picks the language server for it.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub path: Option<String>,

    /// Source text quoted verbatim from `path`, occurring exactly once in
    /// it; an alternative to `symbol`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub anchor: Option<String>,

    /// The token within `anchor` that is meant, occurring once in the
    /// anchor; the whole anchor when absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub focus: Option<String>,
}

impl ToolMeta for Uses {
    fn title() -> Option<&'static str> {
        Some("Find references to a code symbol")
    }

    fn annotations() -> Option<ToolAnnotations> {
        Some(ToolAnnotations {
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(false),
            ..Default::default()
        })
    }
}

impl Tool<State> for Uses {
    type Output = String;

    fn execute(self, state: &mut State, _context: &RequestContext) -> Result<Self::Output> {
        let root = state.root().to_path_buf();
        let target = Target {
            symbol: self.symbol.as_deref(),
            path: self.path.as_deref(),
            anchor: self.anchor.as_deref(),
            focus: self.focus.as_deref(),
        };
        match resolve_target(state, target)? {
            Resolved::Listed(listing) => Ok(listing),
            Resolved::One(found) => references_of(state, &root, &found),
        }
    }
}

/// The references of one identity, rendered.
fn references_of(state: &mut State, root: &Path, found: &Found) -> Result<String> {
    let (_, synced) = sync_target(state, root, found)?;
    let server = server_named(state, &found.identity.server)?;
    let target = &found.target;
    let symbol = found.name.as_str();
    // The header names the item as `show` would.
    let outline = server.document_symbol(&synced.uri)?;
    let described = describe(server, outline.as_ref(), found)?;
    let header = header(
        described.kind_label,
        &described.name,
        &location_line(root, &target.uri, target.selection_range.start),
    );

    // The sync above precedes this on the wire; wait again in case it
    // started work.
    server.wait_ready()?;
    let raw = server.references(&synced.uri, target.selection_range.start)?;
    let hits = parse_references_result(raw)
        .with_context(|| format!("{} returned an unrecognizable references result", server.name()))?;
    let grouped = group_by_file(hits, root, &target.uri, target.selection_range);
    let total = grouped.total();
    let file_count = grouped.files.len();

    let mut production = Vec::new();
    let mut tests = Vec::new();
    let mut notes = Vec::new();
    let mut no_outline = false;
    let mut files = grouped.files;
    files.sort_by(|a, b| a.path.cmp(&b.path));
    for file in files {
        let (lines, outlined) = file_lines(server, root, symbol, &file)?;
        no_outline |= !outlined;
        let display = relative_display(root, &file.path);
        let path_is_test = classify::path_is_test(&file.path);
        let (test_hits, other_hits): (Vec<HitLine>, Vec<HitLine>) = lines
            .into_iter()
            .partition(|line| path_is_test || line.is_test);
        for (bucket, hits) in [(&mut production, other_hits), (&mut tests, test_hits)] {
            if !hits.is_empty() {
                bucket.push(FileGroup {
                    path: display.clone(),
                    hits,
                });
            }
        }
    }
    if no_outline {
        notes.push(format!(
            "{} does not support documentSymbol; hits carry no enclosing item",
            server.name()
        ));
    }
    if grouped.out_of_root > 0 {
        notes.push(format!(
            "{} reference(s) outside the workspace root were excluded",
            grouped.out_of_root
        ));
    }
    Ok(render_uses(
        &header, total, file_count, &production, &tests, MAX_HITS, &notes,
    ))
}

/// Read one file with hits from disk, check every hit against that text,
/// and build its lines: 1-based line, enclosing item, trimmed source. The
/// bool says whether the server gave an outline at all.
///
/// The file is read, not synced: syncing would make every file a broad
/// query touches an owned document for the rest of the session. The hits
/// came from the server's view; what is checked is that the disk text
/// agrees with it at every range.
fn file_lines(
    server: &LspServer,
    root: &Path,
    symbol: &str,
    file: &FileHits,
) -> Result<(Vec<HitLine>, bool)> {
    let encoding = server.encoding();
    let aliases = server.adapter().reference_aliases();
    let text = std::fs::read_to_string(&file.path)
        .with_context(|| format!("reading {} for its references", file.path.display()))?;
    let outline = server.document_symbol(file.uri.as_str())?;
    let mut lines = Vec::with_capacity(file.hits.len());
    for hit in &file.hits {
        verify_reference_at(&text, hit.range, symbol, &aliases, encoding).with_context(|| {
            format!(
                "{}'s reference to '{symbol}' at {}:{} does not match the file on disk; the \
                 server's view was stale — retry after it re-indexes",
                server.name(),
                relative_display(root, &file.path),
                hit.range.start.line + 1
            )
        })?;
        let chain = match &outline {
            Some(outline) => outline::chain_at(outline, hit.range.start).with_context(|| {
                format!(
                    "{} returned an unusable documentSymbol result",
                    server.name()
                )
            })?,
            None => Vec::new(),
        };
        lines.push(HitLine {
            line: hit.range.start.line + 1,
            item: classify::item_label(&chain),
            is_test: classify::chain_is_test(&chain),
            text: source_line(&text, hit.range.start, encoding),
        });
    }
    lines.sort_by_key(|line| line.line);
    Ok((lines, outline.is_some()))
}

/// The line holding `position`, trimmed; long lines are cut with a marker.
fn source_line(text: &str, position: Position, encoding: Encoding) -> String {
    const MAX_CHARS: usize = 160;
    let line = position_to_offset(text, Position::new(position.line, 0), encoding)
        .ok()
        .map(|start| text[start..].lines().next().unwrap_or_default())
        .unwrap_or_default()
        .trim();
    if line.chars().count() <= MAX_CHARS {
        line.to_string()
    } else {
        let mut cut: String = line.chars().take(MAX_CHARS).collect();
        cut.push('…');
        cut
    }
}


#[cfg(test)]
mod tests;
