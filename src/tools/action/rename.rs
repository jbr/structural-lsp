//! The rename flow: resolve, ask the server for the edit, check it against
//! disk, preview or apply.

use super::Mode;
use super::render::{FileChange, Prepared, render_applied, render_preview};
use crate::lsp::edits::{self, FileRename, Located, Plan};
use crate::state::State;
use crate::tools::listing::relative_display;
use crate::tools::resolution::{Resolved, Target, resolve_target, server_named, sync_target};
use anyhow::{Context, Result, bail};
use lsp_types::WorkspaceEdit;
use serde_json::Value;
use std::path::Path;

pub(super) fn run(
    state: &mut State,
    target: Target<'_>,
    new_name: &str,
    mode: Mode,
    digest: Option<&str>,
) -> Result<String> {
    if new_name.is_empty() || new_name.chars().any(char::is_whitespace) {
        bail!("`new_name` must be a single name without whitespace");
    }
    let root = state.root().to_path_buf();
    let found = match resolve_target(state, target)? {
        Resolved::Listed(listing) => return Ok(listing),
        Resolved::One(found) => *found,
    };
    let header = crate::tools::show::target_header(state, &root, &found)?;
    let (_, synced) = sync_target(state, &root, &found)?;
    let server = server_named(state, &found.identity.server)?;
    if !server.supports("renameProvider") {
        return Ok(format!(
            "{header}\nrename is not supported by {}\n",
            server.name()
        ));
    }
    let old_name = found.name.clone();
    if old_name == new_name {
        bail!("`new_name` is already the name");
    }
    // A module whose definition is its whole file has no name token there;
    // the rename goes to where the module is declared (the origin).
    let (uri, position) = if crate::lsp::definition::is_whole_file(
        &synced.text,
        found.target.selection_range,
        server.encoding(),
    ) {
        let origin = &found.identity.origin.location;
        let path = crate::lsp::definition::candidate_path(&origin.uri)
            .context("the origin of a file-level definition is not a file path")?;
        let origin_synced = server.sync(&path)?;
        (origin_synced.uri, origin.range.start)
    } else {
        (synced.uri.clone(), found.target.selection_range.start)
    };
    server.wait_ready()?;
    // The server may say nothing here can be renamed; that is the answer.
    let prepares = server
        .capabilities()
        .pointer("/renameProvider/prepareProvider")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    if prepares && server.prepare_rename(&uri, position)?.is_null() {
        return Ok(format!(
            "{header}\nnot applicable: {} cannot rename `{old_name}` here\n",
            server.name()
        ));
    }
    let raw = server.rename(&uri, position, new_name)?;
    if raw.is_null() {
        return Ok(format!(
            "{header}\nno edits: {} returned nothing for renaming `{old_name}`\n",
            server.name()
        ));
    }
    let edit: WorkspaceEdit = serde_json::from_value(raw)
        .with_context(|| format!("{} returned an unrecognizable rename result", server.name()))?;
    let plan = edits::normalize(&edit)?;
    if plan.edit_count() == 0 && plan.renames.is_empty() {
        return Ok(format!(
            "{header}\nno edits: {} returned an empty edit for renaming `{old_name}`\n",
            server.name()
        ));
    }
    let encoding = server.encoding();
    let server_name = server.name().to_string();
    let prepared = prepare(&root, &plan, &old_name, encoding, &server_name)?;
    let title = format!("rename {old_name} → {new_name} · {header}");

    match mode {
        Mode::Preview => Ok(render_preview(&title, &prepared)),
        Mode::Apply => {
            if digest != Some(prepared.digest.as_str()) {
                let why = match digest {
                    None => "no digest given".to_string(),
                    Some(_) => "the digest does not match: something the rename touches changed since the preview".to_string(),
                };
                return Ok(format!(
                    "nothing applied: {why}; a fresh preview follows\n{}",
                    render_preview(&title, &prepared)
                ));
            }
            apply(state, &root, &found.identity.server, &prepared)?;
            Ok(render_applied(&title, &prepared))
        }
    }
}

/// Read every file the plan touches, check each edit against that text,
/// and compute the new texts, the changed lines and the digest. Nothing is
/// written.
fn prepare(
    root: &Path,
    plan: &Plan,
    old_name: &str,
    encoding: crate::lsp::positions::Encoding,
    server: &str,
) -> Result<Prepared> {
    let mut files = Vec::with_capacity(plan.files.len());
    let mut digest_inputs: Vec<(&std::path::PathBuf, String, &[lsp_types::TextEdit])> = Vec::new();
    for file in &plan.files {
        let text = std::fs::read_to_string(&file.path)
            .with_context(|| format!("reading {} for the rename", file.path.display()))?;
        let located: Vec<Located> = edits::locate(&text, &file.edits, encoding)
            .with_context(|| format!("{}'s edit to {}", server, relative_display(root, &file.path)))?;
        for edit in &located {
            let current = &text[edit.start..edit.end];
            if !current.is_empty() && current != old_name {
                bail!(
                    "{server}'s rename edit at {}:{} would replace `{current}`, not `{old_name}`; \
                     the server's view was stale — retry after it re-indexes",
                    relative_display(root, &file.path),
                    text[..edit.start].matches('\n').count() + 1
                );
            }
        }
        let changes = edits::line_changes(&text, &located);
        let after = edits::apply(&text, &located);
        files.push(FileChange {
            path: file.path.clone(),
            display: relative_display(root, &file.path),
            edits: located.len(),
            changes,
            after,
        });
        digest_inputs.push((&file.path, text, &file.edits));
    }
    for rename in &plan.renames {
        if !rename.from.exists() {
            bail!(
                "the rename moves {}, which does not exist",
                relative_display(root, &rename.from)
            );
        }
        if rename.to.exists() {
            bail!(
                "the rename moves {} to {}, which already exists",
                relative_display(root, &rename.from),
                relative_display(root, &rename.to)
            );
        }
    }
    let digest = edits::digest(
        &digest_inputs
            .iter()
            .map(|(path, text, edits)| (*path, text.as_str(), *edits))
            .collect::<Vec<_>>(),
        &plan.renames,
    );
    Ok(Prepared {
        files,
        renames: plan.renames.clone(),
        digest,
    })
}

/// Write the new texts, move the files, and bring every server back in
/// step: a moved file is closed under its old path on whichever server
/// owned it, then every owned document is re-synced and the target's
/// server is waited for, so the next query sees the rename.
fn apply(state: &mut State, root: &Path, server: &str, prepared: &Prepared) -> Result<()> {
    for file in &prepared.files {
        std::fs::write(&file.path, &file.after)
            .with_context(|| format!("writing {}", file.display))?;
    }
    for FileRename { from, to } in &prepared.renames {
        if let Some(parent) = to.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        std::fs::rename(from, to).with_context(|| {
            format!(
                "moving {} to {}",
                relative_display(root, from),
                relative_display(root, to)
            )
        })?;
        for index in 0..state.servers().len() {
            state.server_mut(index).moved(from)?;
        }
    }
    let server = server_named(state, server)?;
    server.sync_owned()?;
    server.wait_ready()?;
    Ok(())
}
