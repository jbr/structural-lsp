//! `action` — a named semantic change at a target, previewed before it is
//! applied. In this slice the action is `rename`.
//!
//! Two identities, kept apart: the *intent* (target, action, arguments) is
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
    types::{RequestContext, ToolAnnotations},
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

mod rename;
mod render;

pub const RENAME: &str = "rename";
pub const PREVIEW: &str = "preview";
pub const APPLY: &str = "apply";

/// Preview or apply a semantic change at a code symbol. Actions: `rename`
/// (with `new_name`), which renames the symbol everywhere the language
/// server knows it is used, across files, following identity rather than
/// text. Target it as `show` does: by name (`symbol`, bare or qualified,
/// with an optional `path` scope) or by quoting source text (`anchor` in
/// `path`, `focus` naming the token).
///
/// `mode = "preview"` (default) shows every changed line and file rename
/// with a `digest`; `mode = "apply"` with that digest recomputes the edit
/// and writes it only if nothing it touches changed since the preview,
/// otherwise it returns a fresh preview and writes nothing. The servers
/// are re-synced after an apply, so the next query sees the new names.
#[derive(Serialize, Deserialize, Debug, JsonSchema, clap::Args)]
#[serde(rename = "action")]
#[group(skip)]
pub struct Action {
    /// The action: `rename`.
    pub action: String,

    /// The symbol's name: bare (`choose`) or qualified by its container
    /// (`Config::new`). Exact match on the name.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub symbol: Option<String>,

    /// A file or directory under the workspace root, relative to it: a
    /// scope for `symbol`, or the file an `anchor` is quoted from.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub path: Option<String>,

    /// Source text quoted verbatim from `path`, occurring exactly once in
    /// it; an alternative to `symbol`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub anchor: Option<String>,

    /// The token within `anchor` that is meant; the whole anchor when
    /// absent.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub focus: Option<String>,

    /// For `rename`: the new name.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub new_name: Option<String>,

    /// `preview` (default) or `apply`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub mode: Option<String>,

    /// The digest a preview returned; required to `apply`.
    #[serde(skip_serializing_if = "Option::is_none")]
    #[arg(long)]
    pub digest: Option<String>,
}

impl ToolMeta for Action {
    fn title() -> Option<&'static str> {
        Some("Preview or apply a semantic change")
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

impl Tool<Session> for Action {
    type Output = String;

    fn execute(self, session: &mut Session, _context: &RequestContext) -> Result<Self::Output> {
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
        match self.action.trim() {
            RENAME => {
                let Some(new_name) = self.new_name.as_deref().map(str::trim) else {
                    bail!("`rename` needs `new_name`");
                };
                rename::run(&mut state, target, new_name, mode, self.digest.as_deref())
            }
            other => bail!("unknown action `{other}`; actions: {RENAME}"),
        }
    }
}
