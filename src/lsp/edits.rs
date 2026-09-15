//! A `WorkspaceEdit` as pure data: normalised into per-file text edits and
//! file renames, applied to text in memory, and summarised for a preview.
//! No I/O; the tool layer reads, checks, and writes.

use crate::lsp::positions::{Encoding, position_to_offset};
use anyhow::{Context, Result, bail};
use lsp_types::{
    DocumentChangeOperation, DocumentChanges, OneOf, ResourceOp, TextEdit, Uri, WorkspaceEdit,
};
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::PathBuf;

/// The text edits one file receives, in the server's order.
#[derive(Debug, Clone, PartialEq)]
pub struct FileEdits {
    pub uri: Uri,
    pub path: PathBuf,
    pub edits: Vec<TextEdit>,
}

/// A file the edit moves.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileRename {
    pub from: PathBuf,
    pub to: PathBuf,
}

/// Everything a `WorkspaceEdit` asks for, in the order given.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Plan {
    pub files: Vec<FileEdits>,
    pub renames: Vec<FileRename>,
}

impl Plan {
    pub fn edit_count(&self) -> usize {
        self.files.iter().map(|file| file.edits.len()).sum()
    }
}

/// Fold `changes` and `documentChanges` into one plan. Edits to the same
/// file from both are concatenated; files are ordered by path and each
/// file's edits by range, so the same edit always yields the same plan
/// (and digest) whatever order the server listed it in — gopls varies it
/// between calls. `create`/`delete` operations are refused, stated:
/// nothing here applies them.
pub fn normalize(edit: &WorkspaceEdit) -> Result<Plan> {
    let mut plan = Plan::default();
    let mut push = |uri: &Uri, edits: Vec<TextEdit>| -> Result<()> {
        let path = crate::lsp::definition::candidate_path(uri)
            .with_context(|| format!("edit target {} is not a file path", uri.as_str()))?;
        match plan.files.iter_mut().find(|file| file.path == path) {
            Some(file) => file.edits.extend(edits),
            None => plan.files.push(FileEdits {
                uri: uri.clone(),
                path,
                edits,
            }),
        }
        Ok(())
    };
    if let Some(changes) = &edit.changes {
        let mut uris: Vec<&Uri> = changes.keys().collect();
        uris.sort_by(|a, b| a.as_str().cmp(b.as_str()));
        for uri in uris {
            push(uri, changes[uri].clone())?;
        }
    }
    match &edit.document_changes {
        None => {}
        Some(DocumentChanges::Edits(edits)) => {
            for document in edits {
                push(&document.text_document.uri, plain(&document.edits))?;
            }
        }
        Some(DocumentChanges::Operations(operations)) => {
            for operation in operations {
                match operation {
                    DocumentChangeOperation::Edit(document) => {
                        push(&document.text_document.uri, plain(&document.edits))?;
                    }
                    DocumentChangeOperation::Op(ResourceOp::Rename(rename)) => {
                        let from = crate::lsp::definition::candidate_path(&rename.old_uri)
                            .context("rename source is not a file path")?;
                        let to = crate::lsp::definition::candidate_path(&rename.new_uri)
                            .context("rename target is not a file path")?;
                        plan.renames.push(FileRename { from, to });
                    }
                    DocumentChangeOperation::Op(ResourceOp::Create(create)) => bail!(
                        "the edit creates {}; file creation is not supported",
                        create.uri.as_str()
                    ),
                    DocumentChangeOperation::Op(ResourceOp::Delete(delete)) => bail!(
                        "the edit deletes {}; file deletion is not supported",
                        delete.uri.as_str()
                    ),
                }
            }
        }
    }
    plan.files.sort_by(|a, b| a.path.cmp(&b.path));
    for file in &mut plan.files {
        file.edits.sort_by_key(|edit| {
            (
                edit.range.start.line,
                edit.range.start.character,
                edit.range.end.line,
                edit.range.end.character,
            )
        });
    }
    plan.renames.sort_by(|a, b| a.from.cmp(&b.from));
    Ok(plan)
}

fn plain(edits: &[OneOf<TextEdit, lsp_types::AnnotatedTextEdit>]) -> Vec<TextEdit> {
    edits
        .iter()
        .map(|edit| match edit {
            OneOf::Left(edit) => edit.clone(),
            OneOf::Right(annotated) => annotated.text_edit.clone(),
        })
        .collect()
}

/// One edit resolved to byte offsets in a file's text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Located {
    pub start: usize,
    pub end: usize,
    pub new_text: String,
}

/// Resolve edits against `text`, in descending order of start so they can
/// be applied in place. Overlapping edits are an error: the spec forbids
/// them and applying them would invent text.
pub fn locate(text: &str, edits: &[TextEdit], encoding: Encoding) -> Result<Vec<Located>> {
    let mut located = Vec::with_capacity(edits.len());
    for edit in edits {
        let start = position_to_offset(text, edit.range.start, encoding)
            .context("an edit starts outside the file as read from disk")?;
        let end = position_to_offset(text, edit.range.end, encoding)
            .context("an edit ends outside the file as read from disk")?;
        if end < start {
            bail!("an edit's range is inverted");
        }
        located.push(Located {
            start,
            end,
            new_text: edit.new_text.clone(),
        });
    }
    located.sort_by_key(|edit| std::cmp::Reverse((edit.start, edit.end)));
    for pair in located.windows(2) {
        // Descending: pair[1] starts at or before pair[0].
        if pair[1].end > pair[0].start {
            bail!("the edits overlap; refusing to apply them");
        }
    }
    Ok(located)
}

/// `text` with `located` (descending) applied.
pub fn apply(text: &str, located: &[Located]) -> String {
    let mut out = text.to_string();
    for edit in located {
        out.replace_range(edit.start..edit.end, &edit.new_text);
    }
    out
}

/// A changed line for the preview: 1-based line in the old text, the old
/// line, the new line (or `None` when the edit spans lines and the line
/// cannot be shown as one).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineChange {
    pub line: u32,
    pub before: String,
    pub after: Option<String>,
}

/// The lines the edits change, each shown once with every edit on it
/// applied.
pub fn line_changes(text: &str, located: &[Located]) -> Vec<LineChange> {
    let mut changes: Vec<LineChange> = Vec::new();
    // Ascending for reading order.
    let mut edits: Vec<&Located> = located.iter().collect();
    edits.sort_by_key(|edit| (edit.start, edit.end));
    let mut index = 0;
    while index < edits.len() {
        let first = edits[index];
        let line_start = text[..first.start].rfind('\n').map_or(0, |at| at + 1);
        let line_end = text[first.start..]
            .find('\n')
            .map_or(text.len(), |at| first.start + at);
        let line_number = text[..first.start].matches('\n').count() as u32 + 1;
        // Every edit that starts on this line.
        let mut on_line = vec![first];
        let mut next = index + 1;
        while next < edits.len() && edits[next].start <= line_end {
            on_line.push(edits[next]);
            next += 1;
        }
        let multiline = on_line.iter().any(|edit| edit.end > line_end)
            || on_line.iter().any(|edit| edit.new_text.contains('\n'));
        let before = text[line_start..line_end].to_string();
        let after = if multiline {
            None
        } else {
            let mut line = before.clone();
            for edit in on_line.iter().rev() {
                line.replace_range(
                    edit.start - line_start..edit.end - line_start,
                    &edit.new_text,
                );
            }
            Some(line)
        };
        changes.push(LineChange {
            line: line_number,
            before,
            after,
        });
        index = next;
    }
    changes
}

/// A confirmation token over the ordered files, their on-disk text and
/// their edits, plus the renames: the same inputs give the same digest, a
/// changed file or a different edit does not.
pub fn digest(files: &[(&PathBuf, &str, &[TextEdit])], renames: &[FileRename]) -> String {
    let mut hasher = DefaultHasher::new();
    for (path, text, edits) in files {
        path.hash(&mut hasher);
        text.hash(&mut hasher);
        serde_json::to_string(edits)
            .unwrap_or_default()
            .hash(&mut hasher);
    }
    for rename in renames {
        rename.from.hash(&mut hasher);
        rename.to.hash(&mut hasher);
    }
    format!("{:016x}", hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::{Position, Range};
    use serde_json::json;

    fn edit(sl: u32, sc: u32, el: u32, ec: u32, new_text: &str) -> TextEdit {
        TextEdit {
            range: Range::new(Position::new(sl, sc), Position::new(el, ec)),
            new_text: new_text.to_string(),
        }
    }

    #[test]
    fn both_shapes_normalize_and_resource_ops_are_sorted_out() {
        let edit: WorkspaceEdit = serde_json::from_value(json!({
            "changes": { "file:///r/a.rs": [ { "range": { "start": { "line": 0, "character": 3 }, "end": { "line": 0, "character": 9 } }, "newText": "pick" } ] },
            "documentChanges": [
                { "textDocument": { "uri": "file:///r/a.rs", "version": null },
                  "edits": [ { "range": { "start": { "line": 2, "character": 0 }, "end": { "line": 2, "character": 6 } }, "newText": "pick" } ] },
                { "kind": "rename", "oldUri": "file:///r/util.rs", "newUri": "file:///r/helper.rs" }
            ]
        }))
        .unwrap();
        let plan = normalize(&edit).unwrap();
        assert_eq!(plan.files.len(), 1);
        assert_eq!(
            plan.files[0].edits.len(),
            2,
            "both shapes fold into one file"
        );
        assert_eq!(plan.edit_count(), 2);
        assert_eq!(
            plan.renames,
            [FileRename {
                from: "/r/util.rs".into(),
                to: "/r/helper.rs".into()
            }]
        );
        let create: WorkspaceEdit = serde_json::from_value(json!({
            "documentChanges": [ { "kind": "create", "uri": "file:///r/new.rs" } ]
        }))
        .unwrap();
        assert!(
            normalize(&create)
                .unwrap_err()
                .to_string()
                .contains("not supported")
        );
    }

    #[test]
    fn edits_apply_in_place_and_overlaps_are_refused() {
        let text = "fn choose() {}\nchoose();\n";
        let edits = [edit(0, 3, 0, 9, "pick"), edit(1, 0, 1, 6, "pick")];
        let located = locate(text, &edits, Encoding::Utf8).unwrap();
        assert_eq!(apply(text, &located), "fn pick() {}\npick();\n");
        let changes = line_changes(text, &located);
        assert_eq!(
            changes,
            [
                LineChange {
                    line: 1,
                    before: "fn choose() {}".into(),
                    after: Some("fn pick() {}".into())
                },
                LineChange {
                    line: 2,
                    before: "choose();".into(),
                    after: Some("pick();".into())
                },
            ]
        );
        let overlapping = [edit(0, 3, 0, 9, "a"), edit(0, 5, 0, 12, "b")];
        assert!(locate(text, &overlapping, Encoding::Utf8).is_err());
        let past = [edit(9, 0, 9, 1, "a")];
        assert!(locate(text, &past, Encoding::Utf8).is_err());
    }

    #[test]
    fn two_edits_on_one_line_show_once_and_multiline_shows_no_after() {
        let text = "let a = choose(choose(1));\nx\ny\n";
        let edits = [edit(0, 8, 0, 14, "pick"), edit(0, 15, 0, 21, "pick")];
        let located = locate(text, &edits, Encoding::Utf8).unwrap();
        let changes = line_changes(text, &located);
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].after.as_deref(), Some("let a = pick(pick(1));"));
        let spanning = [edit(1, 0, 2, 1, "z")];
        let located = locate(text, &spanning, Encoding::Utf8).unwrap();
        let changes = line_changes(text, &located);
        assert_eq!(changes[0].line, 2);
        assert_eq!(changes[0].after, None);
        assert_eq!(apply(text, &located), "let a = choose(choose(1));\nz\n");
    }

    #[test]
    fn the_digest_follows_its_inputs() {
        let path = PathBuf::from("/r/a.rs");
        let edits = [edit(0, 0, 0, 1, "x")];
        let one = digest(&[(&path, "text", &edits)], &[]);
        assert_eq!(one, digest(&[(&path, "text", &edits)], &[]));
        assert_ne!(one, digest(&[(&path, "text2", &edits)], &[]));
        assert_ne!(
            one,
            digest(&[(&path, "text", &[edit(0, 0, 0, 1, "y")])], &[])
        );
        assert_ne!(
            one,
            digest(
                &[(&path, "text", &edits)],
                &[FileRename {
                    from: "/a".into(),
                    to: "/b".into()
                }]
            )
        );
        assert_eq!(one.len(), 16);
    }
}
