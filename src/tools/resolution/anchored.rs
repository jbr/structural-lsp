//! Resolution from an anchor: quoted source text in a file, and a focus
//! within it, to the identity `textDocument/definition` gives at the
//! focus. No symbol index is consulted; the file is synced first so the
//! server answers against the text the anchor was located in.

use super::{Exclusions, Resolved, finish};
use crate::lsp::anchor;
use crate::lsp::definition::{
    DefinitionSet, DefinitionTarget, Identity, Origin, candidate_path, is_whole_file,
    parse_definition_result,
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
    /// Byte offset of the focus's start in `text`.
    pub offset: usize,
    pub focus_text: String,
    /// 1-based line of the anchor.
    pub line: u32,
}

/// What an anchored target names: the file, the quoted text, the token in
/// it, and the language the call is narrowed to.
pub struct Anchored<'a> {
    pub scope: Option<PathBuf>,
    pub anchor: &'a str,
    pub focus: Option<&'a str>,
    pub language: Option<&'a str>,
}

/// Sync the anchor's file with the server for its language and locate the
/// focus in the text just read. The server has been waited for; the sync
/// precedes any query the caller sends.
pub fn locate_focus(state: &mut Workspace, root: &Path, anchored: Anchored<'_>) -> Result<Focus> {
    let Anchored {
        scope,
        anchor,
        focus,
        language,
    } = anchored;
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
    let index = state.select(Some(&file), language)?[0];
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
        offset: selection.start,
        focus_text,
        line: selection.line,
    })
}

pub(super) fn from_anchor(
    state: &mut Workspace,
    root: &Path,
    anchored: Anchored<'_>,
) -> Result<Resolved> {
    let located = locate_focus(state, root, anchored)?;
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
    let (names, identities): (Vec<String>, Vec<Identity>) = targets
        .into_iter()
        .map(|target| {
            let name = name_at(&target, encoding).unwrap_or_else(|| focus_text.clone());
            let identity = Identity::anchored(
                &server_name,
                Origin {
                    name: name.clone(),
                    kind: None,
                    container: None,
                    location: origin_location.clone(),
                },
                Some(target),
            );
            (name, identity)
        })
        .unzip();
    // The name is what the definition carries, not how the anchor spelled
    // it. The two differ whenever the anchor is wider than the token —
    // `config.doubled()` resolves to `doubled`, and an anchor given with
    // no `focus` is the whole quoted text, so `choose(Mode::Fast);`
    // resolves to `choose`. Passing the quoted text here would send it
    // into `verify_name_at`, which would then report that the server's
    // target "does not name it in the file on disk" — accusing the server
    // of a mismatch this client constructed (docs/resolution.md).
    //
    // Only a single target has a resolved name to use; an ambiguous
    // result is rendered from the quoted text, which is what the caller
    // wrote and can recognize.
    let name = match names.as_slice() {
        [one] => one.clone(),
        _ => focus_text.clone(),
    };
    finish(
        state,
        root,
        identities,
        &name,
        &focus_text,
        Exclusions::default(),
        &[],
    )
}

/// The text at the target's own range, read from disk: what the focus
/// resolved to is named by its definition, not by how it was spelled at
/// the anchor (`config.doubled()` resolves to `doubled`). A range that is
/// the whole file — a module whose definition is its file — names no
/// token, and the focus is the name.
fn name_at(target: &DefinitionTarget, encoding: Encoding) -> Option<String> {
    let text = std::fs::read_to_string(candidate_path(&target.uri)?).ok()?;
    if is_whole_file(&text, target.selection_range, encoding) {
        return None;
    }
    let start = position_to_offset(&text, target.selection_range.start, encoding).ok()?;
    let end = position_to_offset(&text, target.selection_range.end, encoding).ok()?;
    (start < end).then(|| text[start..end].to_string())
}
