//! `view = "implementations"`: `textDocument/implementation` at the target,
//! rendered as `uses` renders hits (file, `line · item · source`).

use super::unsupported;
use crate::lsp::definition::{DefinitionSet, parse_definition_result};
use crate::lsp::references::{Hit, group_by_file};
use crate::workspace::Workspace;
use crate::tools::hits::{FileGroup, collapse_identical, lines_for, render_groups};
use crate::tools::listing::relative_display;
use crate::tools::resolution::{Resolved, Target, resolve_target, server_named, sync_target};
use anyhow::{Context, Result};
use std::path::Path;

pub(super) fn implementations(state: &mut Workspace, root: &Path, target: Target<'_>) -> Result<String> {
    let found = match resolve_target(state, target)? {
        Resolved::Listed(listing) => return Ok(listing),
        Resolved::One(found) => *found,
    };
    let header = super::target_header(state, root, &found)?;
    let (_, synced) = sync_target(state, root, &found)?;
    let server = server_named(state, &found.identity.server)?;
    if !server.supports("implementationProvider") {
        return Ok(unsupported("implementations", server.name()));
    }
    server.wait_ready()?;
    let raw = server.implementation(&synced.uri, found.target.selection_range.start)?;
    let targets = match parse_definition_result(raw)
        .with_context(|| format!("{} returned an unrecognizable implementation result", server.name()))?
    {
        DefinitionSet::None => Vec::new(),
        DefinitionSet::One(target) => vec![target],
        DefinitionSet::Many(targets) => targets,
    };
    let hits: Vec<Hit> = targets
        .into_iter()
        .map(|target| Hit {
            uri: target.uri,
            range: target.selection_range,
        })
        .collect();
    let grouped = group_by_file(hits, root, &found.target.uri, found.target.selection_range);
    let total = grouped.total();
    if total == 0 {
        return Ok(format!("{header} · no implementations\n"));
    }
    let mut files = grouped.files;
    files.sort_by(|a, b| a.path.cmp(&b.path));
    let mut groups = Vec::with_capacity(files.len());
    for file in &files {
        let (hits, _) = lines_for(server, root, file, None)?;
        groups.push(FileGroup {
            path: relative_display(root, &file.path),
            hits,
        });
    }
    let mut text = format!(
        "{header} · {total} implementation{} · {} file{}\n",
        if total == 1 { "" } else { "s" },
        files.len(),
        if files.len() == 1 { "" } else { "s" }
    );
    // A derive is several implementations at one location and the server
    // says nothing about which is which, so the identical rows are folded
    // into the one that carries the `#[derive(…)]` line naming them. The
    // header keeps the server's count (crate::tools::hits).
    let collapsed = collapse_identical(&mut groups);
    let mut budget = super::MAX_LINES;
    let omitted = render_groups(&mut text, &groups, &mut budget);
    if collapsed > 0 {
        text.push_str(&format!(
            "note: {collapsed} more reported at a line already shown, with nothing \
             distinguishing them\n"
        ));
    }
    if omitted > 0 {
        text.push_str(&format!(
            "note: {omitted} more not shown (listing cap {})\n",
            super::MAX_LINES
        ));
    }
    if grouped.out_of_root > 0 {
        text.push_str(&format!(
            "note: {} implementation(s) outside the workspace root were excluded\n",
            grouped.out_of_root
        ));
    }
    Ok(text)
}
