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

use crate::lsp::references::{group_by_file, parse_references_result};
use crate::session::Session;
use crate::tools::listing::{header, location_line, relative_display};
use crate::tools::resolution::{
    Found, Resolved, Target, describe, resolve_target, server_named, sync_target,
};
use crate::workspace::Workspace;
use anyhow::{Context, Result};
use mcplease::{
    traits::{Tool, ToolMeta},
    types::{RequestContext, ToolAnnotations},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::Path;

mod render;
use crate::tools::hits::{FileGroup, HitLine, OutlineGap, lines_for};
use render::render_uses;

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
/// and same-named strangers are not. Owned files are re-synced from disk
/// and the server is waited for before every query. An anchor resolves
/// inside its `path` file, and a `symbol` with a `path` naming a file
/// nominates from that file's own outline; a bare `symbol`, or a
/// directory-scoped one, is a workspace-index lookup that can miss a
/// recent edit to a file no query has opened. `path` scopes where the
/// *symbol* is looked up, not where its references are listed.
#[derive(Serialize, Deserialize, Debug, JsonSchema, clap::Args)]
#[serde(rename = "uses")]
#[group(skip)]
pub struct Uses {
    /// The symbol's name: bare (`choose`) or qualified by its container
    /// (`Config::new`, `Config.doubled`). Exact match on the name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,

    /// A file or directory under the workspace root, relative to it. A file
    /// nominates `symbol` from that file's own outline rather than the
    /// workspace index, and picks its language server; a directory narrows
    /// the index. With `anchor`, the file the quoted text is in.
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

impl Tool<Session> for Uses {
    type Output = String;

    fn execute(self, session: &mut Session, _context: &RequestContext) -> Result<Self::Output> {
        let mut state = session.acquire(self.path.as_deref())?;
        let root = state.root().to_path_buf();
        let target = Target {
            symbol: self.symbol.as_deref(),
            path: self.path.as_deref(),
            anchor: self.anchor.as_deref(),
            focus: self.focus.as_deref(),
        };
        match resolve_target(&mut state, target)? {
            Resolved::Listed(listing) => Ok(listing),
            Resolved::One(found) => references_of(&mut state, &root, &found),
        }
    }
}

/// The references of one identity, rendered.
fn references_of(state: &mut Workspace, root: &Path, found: &Found) -> Result<String> {
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
    let hits = parse_references_result(raw).with_context(|| {
        format!(
            "{} returned an unrecognizable references result",
            server.name()
        )
    })?;
    let grouped = group_by_file(hits, root, &target.uri, target.selection_range);
    let total = grouped.total();
    let file_count = grouped.files.len();

    let mut production = Vec::new();
    let mut tests = Vec::new();
    let mut notes = Vec::new();
    let mut outline_gap: Option<OutlineGap> = None;
    let mut files = grouped.files;
    files.sort_by(|a, b| a.path.cmp(&b.path));
    for file in files {
        let (lines, gap) = lines_for(server, root, &file, Some(symbol))?;
        // The first gap is the one reported: a second file refused for the
        // same reason adds nothing, and one note per file would bury the
        // hits the query was asked for.
        outline_gap = outline_gap.or(gap);
        let display = relative_display(root, &file.path);
        let path_is_test = crate::tools::classify::path_is_test(&file.path);
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
    match &outline_gap {
        Some(OutlineGap::Unsupported) => notes.push(format!(
            "{} does not support documentSymbol; hits carry no enclosing item",
            server.name()
        )),
        Some(OutlineGap::Refused(why)) => notes.push(format!(
            "{} would not outline every file with hits ({why}); those hits carry no \
             enclosing item",
            server.name()
        )),
        None => {}
    }
    if grouped.out_of_root > 0 {
        notes.push(format!(
            "{} reference(s) outside the workspace root were excluded",
            grouped.out_of_root
        ));
    }
    Ok(render_uses(
        &header,
        total,
        file_count,
        &production,
        &tests,
        MAX_HITS,
        &notes,
    ))
}

#[cfg(test)]
mod tests;
