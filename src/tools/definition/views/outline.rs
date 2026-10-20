//! `view = "outline"`: a file's symbol tree, a target's own children, or a
//! directory's files with their top-level items — one line per symbol,
//! bodies omitted, a function's locals left out, paged past 200 lines
//! (docs/tools.md).

use super::unsupported;
use crate::lsp::outline::{self, Node};
use crate::lsp::server::LspServer;
use crate::tools::listing::relative_display;
use crate::tools::resolution::{Resolved, Target, resolve_target, server_named, sync_target};
use crate::workspace::Workspace;
use anyhow::{Context, Result, bail};
use std::path::Path;

mod directory;
mod locals;
mod page;
#[cfg(test)]
mod tests;

use page::Line;

pub(super) fn run(
    state: &mut Workspace,
    root: &Path,
    target: Target<'_>,
    page: Option<usize>,
) -> Result<String> {
    let number = page.unwrap_or(1);
    page::start_of(number)?;
    if target.symbol.is_none() && target.anchor.is_none() {
        return path_outline(state, root, target.path, target.language, number);
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
    let node =
        outline::subtree_at(&result, found.target.selection_range.start).with_context(|| {
            format!(
                "{} returned an unusable documentSymbol result",
                server.name()
            )
        })?;
    let mut children = node.map(|node| node.children).unwrap_or_default();
    if children.is_empty() {
        return Ok(format!("{header}\nno children in the outline\n"));
    }
    // The target's own children were asked for by name, so a function's
    // locals are listed; the locals of anything below them are not.
    let omitted = locals::strip_below(&mut children);
    // Indented under the header line, which names the target.
    let mut lines = Vec::new();
    push_lines(&mut lines, server, &children, 1);
    let page = page::page(&lines, number, false)?;
    let mut text = format!(
        "{header} · {} children{}{}\n",
        children.len(),
        locals_note(omitted),
        page.label(Some(page::count(lines.len())))
    );
    page.render(&mut text);
    text.push_str(&page.continuation(&format!(
        "{} more line(s)",
        lines.len() - (page::start_of(number)? + page.lines.len())
    )));
    Ok(text)
}

/// The outline `path` names: a file's whole tree, or a directory's files.
fn path_outline(
    state: &mut Workspace,
    root: &Path,
    path: Option<&str>,
    language: Option<&str>,
    number: usize,
) -> Result<String> {
    let Some(path) = path else {
        bail!("`outline` needs `path` naming a file or directory, or a `symbol`/`anchor` target");
    };
    let named = root
        .join(path)
        .canonicalize()
        .with_context(|| format!("`{path}` does not exist under the workspace root"))?;
    if !named.starts_with(root) {
        bail!("`{path}` escapes the workspace root");
    }
    if named.is_dir() {
        return directory::outline(state, root, &named, language, number);
    }
    file_outline(state, root, &named, language, number)
}

/// The whole outline of `file`, locals left out, one page of it.
fn file_outline(
    state: &mut Workspace,
    root: &Path,
    file: &Path,
    language: Option<&str>,
    number: usize,
) -> Result<String> {
    let index = state.select(Some(file), language)?[0];
    let server = state.server_mut(index)?;
    server.sync_owned()?;
    server.wait_ready()?;
    let display = relative_display(root, file);
    let Some(mut nodes) = file_nodes(server, file)? else {
        return Ok(unsupported("outline", server.name()));
    };
    let omitted = locals::strip(&mut nodes, None);
    if nodes.is_empty() {
        return Ok(format!("{display} · empty outline{}\n", locals_note(omitted)));
    }
    let mut lines = Vec::new();
    push_lines(&mut lines, server, &nodes, 0);
    let page = page::page(&lines, number, false)?;
    let count = lines.len();
    let mut text = format!(
        "{display} · {count} symbol{}{}{}\n",
        if count == 1 { "" } else { "s" },
        locals_note(omitted),
        page.label(Some(page::count(count)))
    );
    page.render(&mut text);
    let shown = page::start_of(number)? + page.lines.len();
    text.push_str(&page.continuation(&format!("{} more symbol(s)", count - shown)));
    Ok(text)
}

/// `file`'s outline from `server`, after syncing it from disk: `None` when
/// the server has no `documentSymbol`.
fn file_nodes(server: &mut LspServer, file: &Path) -> Result<Option<Vec<Node>>> {
    let synced = server.sync(file)?;
    let Some(result) = server.document_symbol(&synced.uri)? else {
        return Ok(None);
    };
    outline::tree(&result).map(Some).with_context(|| {
        format!(
            "{} returned an unusable documentSymbol result",
            server.name()
        )
    })
}

/// ` · N locals not shown`, or nothing when no local was left out.
fn locals_note(omitted: usize) -> String {
    match omitted {
        0 => String::new(),
        1 => " · 1 local not shown".to_string(),
        n => format!(" · {n} locals not shown"),
    }
}

/// One line per node, depth-first: `kind name · line[ · detail]`.
fn push_lines(lines: &mut Vec<Line>, server: &LspServer, nodes: &[Node], depth: usize) {
    for node in nodes {
        lines.push(Line {
            depth,
            text: symbol_line(server, node),
        });
        push_lines(lines, server, &node.children, depth + 1);
    }
}

/// `kind name · line[ · detail]` for one symbol, without indentation.
fn symbol_line(server: &LspServer, node: &Node) -> String {
    let extent = &node.extent;
    let line = extent
        .selection_range
        .map_or(extent.range.start.line, |range| range.start.line)
        + 1;
    let mut text = crate::tools::listing::header(
        Some(server.adapter().kind_label(extent.kind)),
        &extent.name,
        &line.to_string(),
    );
    if let Some(detail) = extent
        .detail
        .as_deref()
        .map(str::trim)
        .filter(|detail| !detail.is_empty())
    {
        text.push_str(&format!(" · {detail}"));
    }
    text
}
