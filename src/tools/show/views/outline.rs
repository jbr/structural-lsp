//! `view = "outline"`: a file's symbol tree, or a target's own children,
//! one line per symbol, bodies omitted.

use super::{MAX_LINES, unsupported};
use crate::lsp::outline::{self, Node};
use crate::lsp::server::LspServer;
use crate::workspace::Workspace;
use crate::tools::listing::relative_display;
use crate::tools::resolution::{Resolved, Target, resolve_target, server_named, sync_target};
use anyhow::{Context, Result, bail};
use std::path::Path;

pub(super) fn run(state: &mut Workspace, root: &Path, target: Target<'_>) -> Result<String> {
    if target.symbol.is_none() && target.anchor.is_none() {
        return file_outline(state, root, target.path);
    }
    let found = match resolve_target(state, target)? {
        Resolved::Listed(listing) => return Ok(listing),
        Resolved::One(found) => *found,
    };
    let (_, synced) = sync_target(state, root, &found)?;
    let server = server_named(state, &found.identity.server)?;
    let Some(result) = server.document_symbol(&synced.uri)? else {
        return Ok(unsupported("outline", server.name()));
    };
    let header = super::target_header(state, root, &found)?;
    let server = server_named(state, &found.identity.server)?;
    let node = outline::subtree_at(&result, found.target.selection_range.start)
        .with_context(|| format!("{} returned an unusable documentSymbol result", server.name()))?;
    let children = node.map(|node| node.children).unwrap_or_default();
    if children.is_empty() {
        return Ok(format!("{header}\nno children in the outline\n"));
    }
    let mut text = format!("{header} · {} children\n", children.len());
    render_nodes(&mut text, server, &children, 1, &mut MAX_LINES.clone());
    Ok(text)
}

/// The whole outline of the file `path` names.
fn file_outline(state: &mut Workspace, root: &Path, path: Option<&str>) -> Result<String> {
    let Some(path) = path else {
        bail!("`outline` needs `path` naming a file, or a `symbol`/`anchor` target");
    };
    let file = root
        .join(path)
        .canonicalize()
        .with_context(|| format!("`{path}` does not exist under the workspace root"))?;
    if !file.starts_with(root) {
        bail!("`{path}` escapes the workspace root");
    }
    if !file.is_file() {
        bail!("`outline` needs `path` to name a file; `{path}` is not one");
    }
    let index = state.select(Some(&file))?[0];
    let server = state.server_mut(index);
    server.sync_owned()?;
    server.wait_ready()?;
    let synced = server.sync(&file)?;
    let Some(result) = server.document_symbol(&synced.uri)? else {
        return Ok(unsupported("outline", server.name()));
    };
    let nodes = outline::tree(&result)
        .with_context(|| format!("{} returned an unusable documentSymbol result", server.name()))?;
    let display = relative_display(root, &file);
    if nodes.is_empty() {
        return Ok(format!("{display} · empty outline\n"));
    }
    let count = count(&nodes);
    let mut text = format!(
        "{display} · {count} symbol{}\n",
        if count == 1 { "" } else { "s" }
    );
    let mut budget = MAX_LINES;
    render_nodes(&mut text, server, &nodes, 0, &mut budget);
    if budget == 0 {
        let omitted = count.saturating_sub(MAX_LINES);
        if omitted > 0 {
            text.push_str(&format!("note: {omitted} more symbol(s) not shown (listing cap {MAX_LINES})\n"));
        }
    }
    Ok(text)
}

fn count(nodes: &[Node]) -> usize {
    nodes.iter().map(|node| 1 + count(&node.children)).sum()
}

/// `kind name · line[ · detail]`, indented by depth; stops when the budget
/// is spent.
fn render_nodes(text: &mut String, server: &LspServer, nodes: &[Node], depth: usize, budget: &mut usize) {
    for node in nodes {
        if *budget == 0 {
            return;
        }
        *budget -= 1;
        let extent = &node.extent;
        let line = extent
            .selection_range
            .map_or(extent.range.start.line, |range| range.start.line)
            + 1;
        text.push_str(&"  ".repeat(depth));
        text.push_str(&crate::tools::listing::header(
            Some(server.adapter().kind_label(extent.kind)),
            &extent.name,
            &line.to_string(),
        ));
        if let Some(detail) = extent
            .detail
            .as_deref()
            .map(str::trim)
            .filter(|detail| !detail.is_empty())
        {
            text.push_str(&format!(" · {detail}"));
        }
        text.push('\n');
        render_nodes(text, server, &node.children, depth + 1, budget);
    }
}
