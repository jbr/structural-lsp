//! `definition` — resolve a symbol by bare name and return its definition's
//! decorated source, not just a location.
//!
//! Resolution is the shared pipeline in [`crate::tools::resolution`]; this
//! module is the body path: own the target's file, check its range, take
//! the item's extent from the server's `documentSymbol`, widen it over
//! leading decorations with tree-sitter where a grammar exists, and render.

use crate::lsp::definition::DefinitionTarget;
use crate::lsp::positions::position_to_offset;
use crate::lsp::server::LspServer;
use crate::lsp::shape;
use crate::session::Session;
use crate::tools::listing::{header, location_line};
use crate::tools::resolution::{
    Described, Found, Resolved, Target, describe, resolve_target, server_named, sync_target,
};
use crate::workspace::Workspace;
use anyhow::{Context, Result};
use mcplease::{
    traits::{Tool, ToolMeta},
    types::{Example, RequestContext, ToolAnnotations},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::Path;

mod render;
mod views;
use render::*;
pub use views::target_header;

/// Upper bound on the returned item body. Omission is noted, never silent;
/// pagination is not implemented in this slice.
const BODY_BUDGET_BYTES: usize = 12_000;

/// Go to definition through the language server (LSP): the replacement for
/// reading a whole file to find one item, or guessing with grep where a name
/// is defined.
///
/// Use when you want a symbol's source: `{"symbol": "Config::new"}` returns
/// its definition with doc comments and attributes, not just a location.
/// Prefer this over `read` when you need one item's body, and over `grep`
/// when the name may be re-exported, shadowed, or implemented across files.
/// The same target selects other IDE views through `view`: `outline` (the
/// target's children, or a whole file's symbols with only `path`),
/// `implementations`, `incoming-calls`, `outgoing-calls`, `type` (the hover
/// signature; with an anchor, the type of the focused expression), and per
/// server `expanded` (rust-analyzer macro expansion at the focus) and
/// `related-tests` (rust-analyzer, with the cargo command for each).
///
/// Target by name (`symbol`, bare like `choose` or qualified by its
/// container like `Config::new`) or by quoting source text (`anchor` in the
/// file `path`, `focus` naming the token meant), which reaches anything a
/// name cannot: locals, call sites, expressions. `anchor` takes quoted
/// source instead of a line number because the model's snapshot of a file
/// may be stale; `focus` names the token inside it (`config` in
/// `let config = Config::new(3);`). Only exact names resolve, and several
/// distinct definitions are listed rather than picked (docs/resolution.md).
///
/// Before each query, every file in the servers' languages that was
/// written, created, or deleted since they last answered is pushed to them
/// from disk, and they are waited for; an edit made just before the call is
/// seen (docs/correctness.md). The workspace root is fixed at server start.
#[derive(Serialize, Deserialize, Debug, JsonSchema, clap::Args)]
#[serde(rename = "definition")]
#[group(skip)]
pub struct Definition {
    /// The symbol's name: bare (`choose`) or qualified by its container
    /// (`Config::new`). Exact match. Start here: `{"symbol": "Config::new"}`
    /// is the whole happy path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,

    /// A file or directory under the workspace root, relative to it. A file
    /// resolves `symbol` from that file's own outline (fresh, and it picks
    /// the language server); a directory narrows the workspace index. With
    /// `anchor`, the file the quoted text is in.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub path: Option<String>,

    /// Source text quoted verbatim from `path`, occurring exactly once in
    /// it; the way to reach anything a name cannot (a local, a call site, an
    /// expression). Quoted text, not a line number, so a stale snapshot of
    /// the file still resolves.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub anchor: Option<String>,

    /// The token within `anchor` that is meant (`config` in `let config =
    /// Config::new(3);`), occurring once in the anchor as a whole
    /// identifier (`config` is not found inside `config_path`); the whole
    /// anchor when absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub focus: Option<String>,

    /// Which IDE view of the same target: `definition` (default), `outline`,
    /// `implementations`, `incoming-calls`, `outgoing-calls`, `type`,
    /// `expanded`, `related-tests`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub view: Option<String>,
}

impl ToolMeta for Definition {
    fn title() -> Option<&'static str> {
        Some("Go to definition (LSP)")
    }

    fn examples() -> Vec<Example<Self>> {
        vec![
            Example {
                description: "Go to a definition by name (Rust)",
                item: Self {
                    symbol: Some("Config::new".to_string()),
                    path: None,
                    anchor: None,
                    focus: None,
                    view: None,
                },
            },
            Example {
                description: "Disambiguate a common name with a file scope (Python)",
                item: Self {
                    symbol: Some("doubled".to_string()),
                    path: Some("helper.py".to_string()),
                    anchor: None,
                    focus: None,
                    view: None,
                },
            },
            Example {
                description: "Reach a call site with quoted source instead of a line number (TypeScript)",
                item: Self {
                    symbol: None,
                    path: Some("main.ts".to_string()),
                    anchor: Some("const offset = choose(true);".to_string()),
                    focus: Some("choose".to_string()),
                    view: None,
                },
            },
        ]
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

impl Tool<Session> for Definition {
    type Output = String;

    async fn execute(self, session: Session, context: RequestContext) -> Result<Self::Output> {
        crate::tools::unblock(&context, move || self.run(&session)).await
    }
}

impl Definition {
    pub fn run(self, session: &Session) -> Result<String> {
        let mut state = session.acquire(self.path.as_deref())?;
        let root = state.view().to_path_buf();
        let target = Target {
            view: &root,
            symbol: self.symbol.as_deref(),
            path: self.path.as_deref(),
            anchor: self.anchor.as_deref(),
            focus: self.focus.as_deref(),
        };
        match self.view.as_deref().map(str::trim) {
            None | Some("") | Some(views::DEFINITION) => {
                match resolve_target(&mut state, target)? {
                    Resolved::Listed(listing) => Ok(listing),
                    Resolved::One(found) => render_definition(&mut state, &root, &found),
                }
            }
            Some(view) => views::run(&mut state, view, target),
        }
    }
}

/// The single identity's body: own the target's file, check its range,
/// shape, and render with any notes.
fn render_definition(state: &mut Workspace, root: &Path, found: &Found) -> Result<String> {
    let (target_path, synced) = sync_target(state, root, found)?;
    let server = server_named(state, &found.identity.server)?;
    let text = synced.text;
    let language = server.language_id(&target_path);
    // Ordered after the sync that precedes this call, so the outline is of
    // the text we read.
    let outline = server.document_symbol(&synced.uri)?;
    let described = describe(server, outline.as_ref(), found)?;
    let (body, mut notes) = shape_body(
        server,
        outline.is_some(),
        &text,
        found,
        &described,
        &language,
    )?;
    if !target_path.starts_with(root) {
        notes.push(format!(
            "definition is outside the workspace root: {}",
            target_path.display()
        ));
    }
    let head = header(
        described.kind_label,
        &described.name,
        &location_line(root, &found.target.uri, found.target.selection_range.start),
    );
    Ok(render_resolved(&head, &body, &notes))
}

/// The item's body, and notes only when it did not come the normal way.
///
/// Normal: the server's `documentSymbol` gives the extent of the target's
/// own symbol, and tree-sitter widens its start over the leading doc
/// comments, attributes or decorators the language keeps outside the item
/// (nothing, for a language without a grammar here). A target that is no
/// outline item (a local) is its definition's line. Fallback, said in a
/// note: with no outline from the server, the Rust grammar's own item
/// search, or else the server's definition range as-is. Never a guessed
/// item.
fn shape_body(
    server: &LspServer,
    outlined: bool,
    text: &str,
    found: &Found,
    described: &Described,
    language: &str,
) -> Result<(String, Vec<String>)> {
    let encoding = server.encoding();
    let target: &DefinitionTarget = &found.target;
    if crate::lsp::definition::is_whole_file(text, target.selection_range, encoding) {
        // A module whose definition is its file: the file is the body.
        let (body, truncated) = budget(text);
        return Ok((body, truncated.into_iter().collect()));
    }
    if let Some(extent) = &described.extent {
        let start = position_to_offset(text, extent.range.start, encoding)
            .context("the documentSymbol range starts outside the file as read from disk")?;
        let end = position_to_offset(text, extent.range.end, encoding)
            .context("the documentSymbol range ends outside the file as read from disk")?;
        if start <= end {
            let start = shape::extend_start(language, text, start)?;
            let (body, truncated) = budget(&text[start..end]);
            return Ok((body, truncated.into_iter().collect()));
        }
    }
    let offset = position_to_offset(text, target.selection_range.start, encoding)
        .context("definition target range is not inside the target file as read from disk")?;
    if outlined {
        // The outline is there but the target is not an item in it: a
        // local, a parameter, a field the server does not list. Its line
        // is the definition.
        let line_start = text[..offset].rfind('\n').map_or(0, |at| at + 1);
        let line = text[line_start..].lines().next().unwrap_or_default().trim();
        return Ok((line.to_string(), Vec::new()));
    }

    let why = format!("{} does not support documentSymbol", server.name());
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
    Ok((body, notes))
}

#[cfg(test)]
mod tests;
