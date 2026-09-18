//! Resolution from an anchor: quoted source text in a file, and a focus
//! within it, to the identity `textDocument/definition` gives at the
//! focus. No symbol index is consulted; the file is synced first so the
//! server answers against the text the anchor was located in.

use super::{Exclusions, Resolved, finish};
use crate::lsp::anchor;
use crate::lsp::definition::{
    DefinitionSet, DefinitionTarget, Identity, Origin, candidate_path, parse_definition_result,
};
use crate::lsp::positions::{Encoding, offset_to_position, position_to_offset};
use crate::tools::listing::relative_display;
use crate::workspace::Workspace;
use anyhow::{Context, Result, bail};
use lsp_types::{Location, Range, Uri};
use std::path::{Path, PathBuf};

/// A located focus: the server for its file, the synced document, and the
/// focus's range in the server's encoding.
pub struct Focus {
    pub server: usize,
    pub file: PathBuf,
    pub uri: String,
    pub text: String,
    pub range: Range,
    pub focus_text: String,
    /// 1-based line of the anchor.
    pub line: u32,
}

/// Sync the anchor's file with the server for its language and locate the
/// focus in the text just read. The server has been waited for; the sync
/// precedes any query the caller sends.
pub fn locate_focus(
    state: &mut Workspace,
    root: &Path,
    scope: Option<PathBuf>,
    anchor: &str,
    focus: Option<&str>,
) -> Result<Focus> {
    let Some(file) = scope else {
        bail!("`anchor` needs `path` naming the file the quoted text is in");
    };
    if !file.is_file() {
        bail!(
            "`path` must name a file when `anchor` is given; `{}` is not one",
            relative_display(root, &file)
        );
    }
    // The first server serving the file's language; `select` errors when
    // none does.
    let index = state.select(Some(&file))?[0];
    let server = state.server_mut(index)?;
    server.sync_owned()?;
    server.wait_ready()?;
    let synced = server.sync(&file)?;
    let selection = anchor::locate(&synced.text, anchor, focus)?;
    let encoding = server.encoding();
    let start = offset_to_position(&synced.text, selection.start, encoding)?;
    let end = offset_to_position(&synced.text, selection.end, encoding)?;
    let focus_text = synced.text[selection.start..selection.end].to_string();
    // The sync precedes the caller's query on the wire; wait again in case
    // it started work.
    server.wait_ready()?;
    Ok(Focus {
        server: index,
        file,
        uri: synced.uri,
        text: synced.text,
        range: Range { start, end },
        focus_text,
        line: selection.line,
    })
}

pub(super) fn from_anchor(
    state: &mut Workspace,
    root: &Path,
    scope: Option<PathBuf>,
    anchor: &str,
    focus: Option<&str>,
) -> Result<Resolved> {
    let located = locate_focus(state, root, scope, anchor, focus)?;
    let Focus {
        server: index,
        file,
        uri: synced_uri,
        range,
        focus_text,
        line,
        ..
    } = located;
    let server = state.server(index)?;
    let encoding = server.encoding();
    let start = range.start;
    let end = range.end;
    let raw = server.definition(&synced_uri, start)?;
    let set = parse_definition_result(raw).with_context(|| {
        format!(
            "{} returned an unrecognizable definition result",
            server.name()
        )
    })?;
    let server_name = server.name().to_string();
    let uri: Uri = serde_json::from_value(serde_json::Value::String(synced_uri.clone()))
        .context("the synced document URI is not a valid URI")?;
    let origin_location = Location {
        uri,
        range: Range { start, end },
    };

    let mut targets: Vec<DefinitionTarget> = match set {
        DefinitionSet::None => Vec::new(),
        DefinitionSet::One(target) => vec![target],
        DefinitionSet::Many(targets) => targets,
    };
    targets.dedup_by(|a, b| a.uri == b.uri && a.selection_range == b.selection_range);
    if targets.is_empty() {
        return Ok(Resolved::Listed(format!(
            "no definition at `{focus_text}` ({}:{})\n",
            relative_display(root, &file),
            line
        )));
    }
    let identities = targets
        .into_iter()
        .map(|target| {
            let name = name_at(&target, encoding).unwrap_or_else(|| focus_text.clone());
            Identity::anchored(
                &server_name,
                Origin {
                    name,
                    kind: None,
                    location: origin_location.clone(),
                },
                Some(target),
            )
        })
        .collect();
    // An anchor resolves the token it names, so the name being
    // resolved and the name shown are the same text.
    finish(
        state,
        root,
        identities,
        &focus_text,
        &focus_text,
        Exclusions::default(),
    )
}

/// The text at the target's own range, read from disk: what the focus
/// resolved to is named by its definition, not by how it was spelled at
/// the anchor (`config.doubled()` resolves to `doubled`).
fn name_at(target: &DefinitionTarget, encoding: Encoding) -> Option<String> {
    let text = std::fs::read_to_string(candidate_path(&target.uri)?).ok()?;
    let start = position_to_offset(&text, target.selection_range.start, encoding).ok()?;
    let end = position_to_offset(&text, target.selection_range.end, encoding).ok()?;
    (start < end).then(|| text[start..end].to_string())
}
