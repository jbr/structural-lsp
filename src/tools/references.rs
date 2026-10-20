//! `references` — references to a symbol resolved by bare name, one line of
//! context per hit, grouped by file and enclosing item, tests set apart.
//!
//! Resolution is the shared pipeline in [`crate::tools::resolution`], so a
//! name that `definition` would list (not found, fuzzy, ambiguous) is listed
//! here the same way.
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
    Found, Resolved, Target, describe, resolve_scope, resolve_target, server_named, sync_target,
};
use crate::workspace::Workspace;
use anyhow::{Context, Result, bail};
use mcplease::{
    traits::{Tool, ToolMeta},
    types::{Example, RequestContext, ToolAnnotations},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::path::Path;

mod render;
use crate::tools::hits::{OutlineGap, gap_note, lines_for, split_tests};
use render::{Listing, render_references};

/// Hits listed before the rest is reported as a count.
const MAX_HITS: usize = 100;

/// Find all references through the language server (LSP): the replacement
/// for `grep "name("` when you want callers and uses.
///
/// Use when you want every use of a symbol: `{"symbol": "Config::new"}`
/// lists one source line per hit with its enclosing item, grouped by file,
/// tests set apart. Prefer this over `grep` because it follows identity,
/// not text: re-exports, renames, and method calls are found and
/// same-named strangers are not. The declaration itself is not listed. An
/// ambiguous or unknown target returns the same listing `definition` gives.
///
/// Target as `definition` does: by name (`symbol`, bare like `choose` or
/// qualified like `Config::new`) or by quoting source text (`anchor` in the
/// file `path`, `focus` naming the token), which reaches locals and
/// parameters; `workspace` and `language` work as they do there. `path`
/// scopes where the symbol is looked up; `within` scopes which references
/// are listed (docs/tools.md). Files are in path order and
/// a listing stops at 100 references: what is left is summarized by
/// directory, largest first, ready to name in `within`, and `page`
/// continues past it. Tests are split out by a filename/module heuristic;
/// an inline `#[cfg(test)]` block elsewhere stays with production code.
#[derive(Serialize, Deserialize, Debug, JsonSchema, clap::Args)]
#[serde(rename = "references")]
#[group(skip)]
pub struct References {
    /// The symbol's name: bare (`choose`) or qualified by its container or
    /// module (`Config::new`, `listing::render`). Exact match. Start here:
    /// `{"symbol": "Config::new"}`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub symbol: Option<String>,

    /// A file or directory under the workspace root, relative to it. A file
    /// resolves `symbol` from that file's own outline (fresh, and it picks
    /// the language server); a directory narrows the workspace index. With
    /// `anchor`, the file the quoted text is in. Scopes the lookup, not the
    /// results.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub path: Option<String>,

    /// Source text quoted verbatim from `path`, occurring exactly once in
    /// it; the way to reach anything a name cannot (a local, a parameter, a
    /// call site). Quoted text, not a line number, so a stale snapshot of
    /// the file still resolves.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub anchor: Option<String>,

    /// The token within `anchor` that is meant, occurring once in the
    /// anchor as a whole identifier (`config` is not found inside
    /// `config_path`); the whole anchor when absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub focus: Option<String>,

    /// A file or directory under the workspace root, relative to it: list
    /// only the references inside it. Counts say how many there are in all.
    /// Unlike `path`, it does not change which symbol is found.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub within: Option<String>,

    /// Which page of a listing past 100 references, counting from 1: the
    /// note at the end of a page names the next.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub page: Option<usize>,

    /// Another project to answer about for this call: a directory, absolute
    /// or relative to the workspace root, whose project's servers answer;
    /// `path` and the paths in the answer are then relative to it.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub workspace: Option<String>,

    /// Ask only the servers for one language, named by id or extension
    /// (`rust`, `python`, `ts`); without it a name is looked up in every
    /// language here and same-named definitions in several are listed.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub language: Option<String>,
}

impl ToolMeta for References {
    fn title() -> Option<&'static str> {
        Some("Find all references (LSP)")
    }

    fn examples() -> Vec<Example<Self>> {
        vec![
            Example {
                description: "Find every use of a symbol (Rust)",
                item: Self {
                    symbol: Some("Config::new".to_string()),
                    path: None,
                    anchor: None,
                    focus: None,
                    within: None,
                    page: None,
                    workspace: None,
                    language: None,
                },
            },
            Example {
                description: "List only the uses inside one directory (Rust)",
                item: Self {
                    symbol: Some("Config::new".to_string()),
                    path: None,
                    anchor: None,
                    focus: None,
                    within: Some("src/agent".to_string()),
                    page: None,
                    workspace: None,
                    language: None,
                },
            },
            Example {
                description: "Disambiguate a common name with a file scope (Python)",
                item: Self {
                    symbol: Some("Config".to_string()),
                    path: Some("helper.py".to_string()),
                    anchor: None,
                    focus: None,
                    within: None,
                    page: None,
                    workspace: None,
                    language: None,
                },
            },
            Example {
                description: "Find the uses of a name at a call site with quoted source (TypeScript)",
                item: Self {
                    symbol: None,
                    path: Some("main.ts".to_string()),
                    anchor: Some("count = identity(count);".to_string()),
                    focus: Some("identity".to_string()),
                    within: None,
                    page: None,
                    workspace: None,
                    language: None,
                },
            },
        ]
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

impl Tool<Session> for References {
    type Output = String;

    async fn execute(self, session: Session, context: RequestContext) -> Result<Self::Output> {
        crate::tools::unblock(&context, move || self.run(&session)).await
    }
}

impl References {
    pub fn run(self, session: &Session) -> Result<String> {
        let session = session.elsewhere(self.workspace.as_deref())?;
        let language = crate::tools::given(self.language.as_deref());
        let mut state = session.acquire(self.path.as_deref(), language)?;
        let root = state.view().to_path_buf();
        let target = Target {
            view: &root,
            symbol: self.symbol.as_deref(),
            path: self.path.as_deref(),
            anchor: self.anchor.as_deref(),
            focus: self.focus.as_deref(),
            language,
        };
        // Checked before resolution, so a mistyped scope is reported as
        // itself rather than after a query it could not narrow.
        let within = self
            .within
            .as_deref()
            .map(|within| {
                resolve_scope(&root, within).with_context(|| {
                    format!("`within` names `{within}`, which is not a file or directory under the workspace root")
                })
            })
            .transpose()?;
        let page = self.page.unwrap_or(1);
        if page == 0 {
            bail!("`page` counts from 1");
        }
        let window = Window {
            within: within.as_deref(),
            page,
        };
        match resolve_target(&mut state, target)? {
            Resolved::Listed(listing) => Ok(listing),
            Resolved::One(found) => references_of(&mut state, &root, &found, window),
        }
    }
}

/// Which of an identity's references are listed: those inside `within`,
/// one page of them.
#[derive(Clone, Copy)]
struct Window<'a> {
    within: Option<&'a Path>,
    page: usize,
}

/// The references of one identity, rendered.
fn references_of(
    state: &mut Workspace,
    root: &Path,
    found: &Found,
    window: Window<'_>,
) -> Result<String> {
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
    let mut grouped = group_by_file(hits, root, &target.uri, target.selection_range);
    let overall = grouped.total();
    if let Some(within) = window.within {
        grouped.files.retain(|file| file.path.starts_with(within));
    }
    let total = grouped.total();
    let file_count = grouped.files.len();
    let pages = total.div_ceil(MAX_HITS).max(1);
    if window.page > pages {
        bail!("`page: {}` is past the end: this listing has {pages} page(s)", window.page);
    }

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
        split_tests(&file.path, &display, lines, &mut production, &mut tests);
    }
    if let Some(gap) = &outline_gap {
        notes.push(gap_note(server.name(), gap));
    }
    if grouped.out_of_root > 0 {
        notes.push(format!(
            "{} reference(s) outside the workspace root were excluded",
            grouped.out_of_root
        ));
    }
    let within_display = window.within.map(|within| scope_display(root, within));
    Ok(render_references(&Listing {
        header: &header,
        total,
        files: file_count,
        within: within_display
            .as_deref()
            .map(|display| (display, overall)),
        production: &production,
        tests: &tests,
        page: window.page,
        cap: MAX_HITS,
        notes: &notes,
    }))
}

/// A scope as the caller would type it back: root-relative, a directory
/// ending in `/`.
fn scope_display(root: &Path, scope: &Path) -> String {
    let relative = relative_display(root, scope);
    let relative = if relative.is_empty() { ".".to_string() } else { relative };
    if scope.is_dir() && !relative.ends_with('/') {
        format!("{relative}/")
    } else {
        relative
    }
}

#[cfg(test)]
mod tests;
