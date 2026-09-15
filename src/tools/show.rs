//! `show` — resolve a symbol by bare name and return its definition's
//! decorated source, not just a location.
//!
//! Resolution is the shared pipeline in [`crate::tools::resolution`]; this
//! module is the body path: own the target's file, check its range, take
//! the item's extent from the server's `documentSymbol`, widen it over
//! leading decorations with tree-sitter where a grammar exists, and render.

use crate::lsp::definition::{DefinitionTarget, Identity};
use crate::lsp::outline::{self, Extent};
use crate::lsp::positions::position_to_offset;
use crate::lsp::server::LspServer;
use crate::lsp::shape;
use crate::state::State;
use crate::tools::listing::location_line;
use crate::tools::resolution::{Found, Resolved, resolve_symbol, server_named, sync_target};
use anyhow::{Context, Result};
use lsp_types::Location;
use mcplease::{
    traits::{Tool, ToolMeta},
    types::{RequestContext, ToolAnnotations},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::Path;

mod render;
use render::*;

/// Upper bound on the returned item body. Omission is noted, never silent;
/// pagination is not implemented in this slice.
const BODY_BUDGET_BYTES: usize = 12_000;

/// Show a code definition by its bare name: the item's source, including
/// its doc comments and attributes, not just a location.
///
/// Candidates come from the language servers' fuzzy symbol index (not
/// proven exhaustive); only exact-name matches resolve, and several distinct
/// definitions are listed rather than picked. The answer is current: owned
/// files are re-synced from disk and the servers are waited for before every
/// query. `path` scopes to a file or directory under the workspace root,
/// which is fixed at server start.
#[derive(Serialize, Deserialize, Debug, JsonSchema, clap::Args)]
#[serde(rename = "show")]
#[group(skip)]
pub struct Show {
    /// Exact bare symbol name (e.g. `Config`, `choose`). Qualified paths
    /// (`Config::new`) are rejected; pass the bare name.
    pub symbol: String,

    /// A file or directory under the workspace root, relative to it, to
    /// narrow candidates to. A file also picks the language server for it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

impl ToolMeta for Show {
    fn title() -> Option<&'static str> {
        Some("Show a code definition")
    }

    fn annotations() -> Option<ToolAnnotations> {
        Some(ToolAnnotations {
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            // The tool touches only the workspace root it was started with.
            open_world_hint: Some(false),
            ..Default::default()
        })
    }
}

impl Tool<State> for Show {
    type Output = String;

    fn execute(self, state: &mut State, _context: &RequestContext) -> Result<Self::Output> {
        let root = state.root().to_path_buf();
        match resolve_symbol(state, &self.symbol, self.path.as_deref())? {
            Resolved::Listed(listing) => Ok(listing),
            Resolved::One(found) => {
                let Found { identity, target } = *found;
                render_definition(state, &root, self.symbol.trim(), &identity, &target)
            }
        }
    }
}

/// The single identity's body: own the target's file, check its range,
/// shape, and render with any notes.
fn render_definition(
    state: &mut State,
    root: &Path,
    symbol: &str,
    identity: &Identity,
    target: &DefinitionTarget,
) -> Result<String> {
    let (target_path, synced) = sync_target(state, root, symbol, identity, target)?;
    let server = server_named(state, &identity.server)?;
    let text = synced.text;
    let language = server.language_id(&target_path);
    let (extent, body, mut notes) = shape_body(server, &synced.uri, &text, target, &language)?;
    if !target_path.starts_with(root) {
        notes.push(format!(
            "definition is outside the workspace root: {}",
            target_path.display()
        ));
    }
    let origin = &identity.candidates[0];
    let kind = extent.as_ref().map_or(origin.info.kind, |extent| extent.kind);
    let name = extent
        .as_ref()
        .map_or(origin.info.name.as_str(), |extent| extent.name.as_str());
    let location = Location {
        uri: target.uri.clone(),
        range: target.selection_range,
    };
    Ok(render_resolved(
        root,
        server.adapter().kind_label(kind),
        name,
        &location,
        &body,
        &notes,
    ))
}

/// The item's extent and body, and notes only when it did not come the
/// normal way.
///
/// Normal: the server's `documentSymbol` gives the extent of the innermost
/// symbol at the definition, and tree-sitter widens its start over the
/// leading doc comments, attributes or decorators the language keeps
/// outside the item (nothing, for a language without a grammar here).
/// Fallback, said in a note: with no extent from the server, the Rust
/// grammar's own item search, or else the server's definition range as-is.
/// Never a guessed item.
fn shape_body(
    server: &LspServer,
    uri: &str,
    text: &str,
    target: &DefinitionTarget,
    language: &str,
) -> Result<(Option<Extent>, String, Vec<String>)> {
    let encoding = server.encoding();
    // Ordered after the sync that precedes this call, so the outline is of
    // the text we read.
    let outline = server.document_symbol(uri)?;
    let extent = match &outline {
        Some(result) => outline::innermost_at(result, target.selection_range.start)
            .with_context(|| {
                format!(
                    "{} returned an unusable documentSymbol result",
                    server.name()
                )
            })?,
        None => None,
    };
    if let Some(extent) = extent {
        let start = position_to_offset(text, extent.range.start, encoding)
            .context("the documentSymbol range starts outside the file as read from disk")?;
        let end = position_to_offset(text, extent.range.end, encoding)
            .context("the documentSymbol range ends outside the file as read from disk")?;
        if start <= end {
            let start = shape::extend_start(language, text, start)?;
            let (body, truncated) = budget(&text[start..end]);
            return Ok((Some(extent), body, truncated.into_iter().collect()));
        }
    }

    let why = match outline {
        None => format!("{} does not support documentSymbol", server.name()),
        Some(_) => format!(
            "{}'s documentSymbol has no symbol at the definition",
            server.name()
        ),
    };
    let offset = position_to_offset(text, target.selection_range.start, encoding)
        .context("definition target range is not inside the target file as read from disk")?;
    let shaped = if language == "rust" {
        shape::item_at(text, offset)?
    } else {
        None
    };
    let (raw, how) = match shaped {
        Some(item) => (
            item.decorated_text(text).to_string(),
            "item found by the Rust grammar instead",
        ),
        None => (
            fallback_body(text, target, encoding).unwrap_or_default(),
            "the server's definition range is shown as-is",
        ),
    };
    let (body, truncated) = budget(&raw);
    let mut notes = vec![format!("{why}; {how}")];
    notes.extend(truncated);
    Ok((None, body, notes))
}

#[cfg(test)]
mod tests;
