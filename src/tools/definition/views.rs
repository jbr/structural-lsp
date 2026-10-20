//! `show`'s views beyond the definition: the dispatcher, the view names,
//! and the three answers an unavailable view gets (unknown name, not
//! supported by the server, not applicable to the target).

use crate::lsp::adapter::ViewSite;
use crate::tools::listing::{header, location_line, relative_display};
use crate::tools::resolution::anchored::locate_focus;
use crate::tools::resolution::{
    Found, Resolved, Target, describe, resolve_target, server_named, sync_target,
};
use crate::workspace::Workspace;
use anyhow::{Context, Result, bail};
use std::path::Path;

mod calls;
mod hover;
mod locations;
mod outline;

pub const DEFINITION: &str = "definition";
pub const OUTLINE: &str = "outline";
pub const IMPLEMENTATIONS: &str = "implementations";
pub const INCOMING_CALLS: &str = "incoming-calls";
pub const OUTGOING_CALLS: &str = "outgoing-calls";
pub const TYPE: &str = "type";

/// The views every server answers, in the order the tool lists them.
pub const STANDARD: [&str; 6] = [
    DEFINITION,
    OUTLINE,
    IMPLEMENTATIONS,
    INCOMING_CALLS,
    OUTGOING_CALLS,
    TYPE,
];

/// Lines of a listing before the rest is a count.
pub const MAX_LINES: usize = 200;

/// Answer `view` for `target`. `definition` is not handled here.
pub fn run(state: &mut Workspace, view: &str, target: Target<'_>) -> Result<String> {
    let root = target.view.to_path_buf();
    match view {
        OUTLINE => outline::run(state, &root, target),
        IMPLEMENTATIONS => locations::implementations(state, &root, target),
        INCOMING_CALLS => calls::run(state, &root, target, true),
        OUTGOING_CALLS => calls::run(state, &root, target, false),
        TYPE => hover::run(state, &root, target),
        other => extra(state, &root, other, target),
    }
}

/// A view an adapter exposes, or the unknown-view answer.
fn extra(state: &mut Workspace, root: &Path, view: &str, target: Target<'_>) -> Result<String> {
    let offered: Vec<String> = state
        .running()
        .filter(|server| server.adapter().extra_views().contains(&view))
        .map(|server| server.name().to_string())
        .collect();
    if offered.is_empty() {
        let mut known: Vec<&str> = STANDARD.to_vec();
        for server in state.running() {
            known.extend(server.adapter().extra_views());
        }
        known.dedup();
        return Ok(format!(
            "unknown view `{view}`; views: {}\n",
            known.join(", ")
        ));
    }
    // An anchor asks at the focus itself (a macro call is expanded where
    // it is written, not at the macro's definition); a name asks at the
    // definition.
    let (index, uri, position, head, site_text) = match target.anchor {
        Some(anchor) => {
            if target.symbol.is_some() {
                bail!("give either `symbol` or `anchor`, not both");
            }
            let scope = target
                .path
                .map(|path| {
                    root.join(path).canonicalize().with_context(|| {
                        format!("`{path}` does not exist under the workspace root")
                    })
                })
                .transpose()?;
            let focus = locate_focus(state, root, scope, anchor, target.focus)?;
            let head = format!(
                "`{}` · {}:{}",
                focus.focus_text,
                relative_display(root, &focus.file),
                focus.line
            );
            (
                focus.server,
                focus.uri,
                focus.range.start,
                head,
                Some((focus.text, focus.offset)),
            )
        }
        None => {
            let found = match resolve_target(state, target)? {
                Resolved::Listed(listing) => return Ok(listing),
                Resolved::One(found) => *found,
            };
            let head = target_header(state, root, &found)?;
            let (_, synced) = sync_target(state, root, &found)?;
            let index = state
                .index_of(&found.identity.server)
                .with_context(|| format!("{} is no longer running", found.identity.server))?;
            (
                index,
                synced.uri,
                found.target.selection_range.start,
                head,
                None,
            )
        }
    };
    let server = state.server(index)?;
    if !offered.iter().any(|name| name == server.name()) {
        return Ok(format!(
            "view `{view}` is not supported by {}\n",
            server.name()
        ));
    }
    let Some((method, params)) = server.adapter().extra_view_request(view, &uri, position) else {
        bail!(
            "{} offers `{view}` but gave no request for it",
            server.name()
        );
    };
    server.wait_ready()?;
    let result = server.request(&method, params)?;
    let site = site_text.as_ref().map(|(text, offset)| ViewSite {
        text,
        offset: *offset,
    });
    let body = server
        .adapter()
        .render_extra_view(view, &result, root, site)?;
    Ok(format!("{head}\n{body}"))
}

/// `show`'s header for a resolved target, from its file's outline.
pub fn target_header(state: &mut Workspace, root: &Path, found: &Found) -> Result<String> {
    let (_, synced) = sync_target(state, root, found)?;
    let server = server_named(state, &found.identity.server)?;
    let outline = server.document_symbol(&synced.uri)?;
    let described = describe(server, outline.as_ref(), found)?;
    Ok(header(
        described.kind_label,
        &described.name,
        &location_line(root, &found.target.uri, found.target.selection_range.start),
    ))
}

/// The "not supported" line for a standard view.
pub(super) fn unsupported(view: &str, server: &str) -> String {
    format!("view `{view}` is not supported by {server}\n")
}
