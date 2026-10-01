//! `rename` — an LSP rename at a target, previewed before it is applied.
//!
//! Two identities, kept apart: the *intent* (target plus new name) is
//! re-resolved on every call; the *prepared edit* is what the server
//! returns for it now, shown as a preview with a digest over the files it
//! touches. `mode = "apply"` recomputes the edit and applies it only when
//! the digest still matches; otherwise it answers with a fresh preview and
//! changes nothing. Edits are results the server hands back, applied here
//! to disk; a server's own `workspace/applyEdit` is declined as always.

use crate::session::Session;
use crate::tools::resolution::Target;
use anyhow::{Result, bail};
use mcplease::{
    traits::{Tool, ToolMeta},
    types::{Example, RequestContext, ToolAnnotations},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

mod op;
mod render;

pub const PREVIEW: &str = "preview";
pub const APPLY: &str = "apply";

/// Rename a symbol everywhere through the language server (LSP): the
/// replacement for hand-editing each use, or for textual find-and-replace.
///
/// Use when a name must change across files: first call with
/// `{"symbol": "choose", "path": "main.ts", "new_name": "pick"}`
/// previews every changed line with a `digest`, then call again with
/// `{"mode": "apply", "digest": "<preview digest>", ...same target...}` to
/// write it. Follows identity rather than text, so re-exports and method
/// calls move and same-named strangers do not. Target as `definition` does:
/// by name (`symbol`, bare or qualified) or by quoting source text
/// (`anchor` in the file `path`, `focus` naming the token).
///
/// `mode = "preview"` (default) shows every changed line and file rename
/// with a `digest`; `mode = "apply"` with that digest recomputes the edit
/// and writes it only if nothing it touches changed since the preview,
/// otherwise it returns a fresh preview and writes nothing. The servers are
/// re-synced after an apply, so the next query sees the new names.
#[derive(Serialize, Deserialize, Debug, JsonSchema, clap::Args)]
#[serde(rename = "rename")]
#[group(skip)]
pub struct Rename {
    /// The symbol's name: bare (`choose`) or qualified by its container
    /// (`Config::new`). Exact match. `{"symbol": "choose", "path":
    /// "main.ts", "new_name": "pick"}` previews the rename.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub symbol: Option<String>,

    /// A file or directory under the workspace root, relative to it: a scope
    /// for `symbol`, or the file an `anchor` is quoted from. Prefer a file
    /// scope: it resolves from that file's fresh outline.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub path: Option<String>,

    /// Source text quoted verbatim from `path`, occurring exactly once in
    /// it; the way to reach anything a name cannot (a local, a parameter).
    /// Quoted text, not a line number, so a stale snapshot still resolves.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub anchor: Option<String>,

    /// The token within `anchor` that is meant; the whole anchor when
    /// absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub focus: Option<String>,

    /// The new name: a single name without whitespace.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub new_name: Option<String>,

    /// `preview` (default) or `apply`. `apply` needs the `digest` the
    /// preview returned.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub mode: Option<String>,

    /// The digest a preview returned; required to `apply`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub digest: Option<String>,
}

impl ToolMeta for Rename {
    fn title() -> Option<&'static str> {
        Some("Rename symbol (LSP)")
    }

    fn examples() -> Vec<Example<Self>> {
        vec![
            Example {
                description: "Preview a rename across files (TypeScript)",
                item: Self {
                    symbol: Some("choose".to_string()),
                    path: Some("main.ts".to_string()),
                    anchor: None,
                    focus: None,
                    new_name: Some("pick".to_string()),
                    mode: None,
                    digest: None,
                },
            },
            Example {
                description: "Apply the previewed rename with the digest it returned",
                item: Self {
                    symbol: Some("choose".to_string()),
                    path: Some("main.ts".to_string()),
                    anchor: None,
                    focus: None,
                    new_name: Some("pick".to_string()),
                    mode: Some("apply".to_string()),
                    digest: Some("DIGEST_FROM_PREVIEW".to_string()),
                },
            },
            Example {
                description: "Preview a rename of a name quoted from its file (Python)",
                item: Self {
                    symbol: None,
                    path: Some("main.py".to_string()),
                    anchor: Some("offset = choose(True)".to_string()),
                    focus: Some("choose".to_string()),
                    new_name: Some("pick".to_string()),
                    mode: None,
                    digest: None,
                },
            },
        ]
    }

    fn annotations() -> Option<ToolAnnotations> {
        Some(ToolAnnotations {
            read_only_hint: Some(false),
            // A rename rewrites names; nothing is deleted.
            destructive_hint: Some(false),
            // A repeated apply with the same digest finds the files
            // changed and previews instead.
            idempotent_hint: Some(false),
            open_world_hint: Some(false),
            ..Default::default()
        })
    }
}

/// Whether the call previews or applies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Preview,
    Apply,
}

impl Tool<Session> for Rename {
    type Output = String;

    async fn execute(self, session: Session, context: RequestContext) -> Result<Self::Output> {
        crate::tools::unblock(&context, move || self.run(&session)).await
    }
}

impl Rename {
    pub fn run(self, session: &Session) -> Result<String> {
        let mut state = session.acquire(self.path.as_deref())?;
        let mode = match self.mode.as_deref().map(str::trim) {
            None | Some("") | Some(PREVIEW) => Mode::Preview,
            Some(APPLY) => Mode::Apply,
            Some(other) => bail!("`mode` must be `{PREVIEW}` or `{APPLY}`, not `{other}`"),
        };
        let view = state.view().to_path_buf();
        let target = Target {
            view: &view,
            symbol: self.symbol.as_deref(),
            path: self.path.as_deref(),
            anchor: self.anchor.as_deref(),
            focus: self.focus.as_deref(),
        };
        let Some(new_name) = self.new_name.as_deref().map(str::trim) else {
            bail!("`rename` needs `new_name`");
        };
        op::run(&mut state, target, new_name, mode, self.digest.as_deref())
    }
}
