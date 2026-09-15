//! Rendering for `action`: the preview (every changed line, every file
//! rename, the digest) and the applied summary.

use crate::lsp::edits::{FileRename, LineChange};
use std::path::PathBuf;

/// One file's prepared change.
pub(super) struct FileChange {
    pub path: PathBuf,
    /// Root-relative.
    pub display: String,
    pub edits: usize,
    pub changes: Vec<LineChange>,
    /// The file's text after the edits.
    pub after: String,
}

/// The whole prepared edit, computed against disk and not yet written.
pub(super) struct Prepared {
    pub files: Vec<FileChange>,
    pub renames: Vec<FileRename>,
    pub digest: String,
}

/// Changed lines listed before the rest is a count.
const MAX_LINES: usize = 200;

pub(super) fn render_preview(title: &str, prepared: &Prepared) -> String {
    let edits: usize = prepared.files.iter().map(|file| file.edits).sum();
    let mut text = format!(
        "{title} · {edits} edit{} · {} file{}\n",
        if edits == 1 { "" } else { "s" },
        prepared.files.len(),
        if prepared.files.len() == 1 { "" } else { "s" }
    );
    let mut budget = MAX_LINES;
    let mut omitted = 0;
    for file in &prepared.files {
        text.push_str(&file.display);
        text.push('\n');
        for change in &file.changes {
            if budget == 0 {
                omitted += 1;
                continue;
            }
            budget -= 1;
            let width = change.line.to_string().len();
            text.push_str(&format!("  {}: {}\n", change.line, change.before.trim_end()));
            match &change.after {
                Some(after) => text.push_str(&format!(
                    "  {}  {}\n",
                    " ".repeat(width),
                    after.trim_end()
                )),
                None => text.push_str(&format!(
                    "  {}  (the edit spans lines)\n",
                    " ".repeat(width)
                )),
            }
        }
    }
    for rename in &prepared.renames {
        text.push_str(&format!(
            "move {} → {}\n",
            rename.from.display(),
            rename.to.display()
        ));
    }
    if omitted > 0 {
        text.push_str(&format!(
            "note: {omitted} more changed line(s) not shown (listing cap {MAX_LINES})\n"
        ));
    }
    text.push_str(&format!("digest: {}\n", prepared.digest));
    text
}

pub(super) fn render_applied(title: &str, prepared: &Prepared) -> String {
    let edits: usize = prepared.files.iter().map(|file| file.edits).sum();
    let mut text = format!(
        "applied: {title} · {edits} edit{} · {} file{}\n",
        if edits == 1 { "" } else { "s" },
        prepared.files.len(),
        if prepared.files.len() == 1 { "" } else { "s" }
    );
    for file in &prepared.files {
        text.push_str(&format!(
            "{} · {} edit{}\n",
            file.display,
            file.edits,
            if file.edits == 1 { "" } else { "s" }
        ));
    }
    for rename in &prepared.renames {
        text.push_str(&format!(
            "moved {} → {}\n",
            rename.from.display(),
            rename.to.display()
        ));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_preview_shows_each_changed_line_twice_and_ends_with_the_digest() {
        let prepared = Prepared {
            files: vec![FileChange {
                path: "/r/src/lib.rs".into(),
                display: "src/lib.rs".into(),
                edits: 2,
                changes: vec![
                    LineChange {
                        line: 43,
                        before: "pub fn choose(mode: Mode) -> usize {".into(),
                        after: Some("pub fn pick(mode: Mode) -> usize {".into()),
                    },
                    LineChange {
                        line: 62,
                        before: "    let offset = choose(Mode::Fast);".into(),
                        after: None,
                    },
                ],
                after: String::new(),
            }],
            renames: vec![FileRename {
                from: "/r/src/util.rs".into(),
                to: "/r/src/helper.rs".into(),
            }],
            digest: "0123456789abcdef".into(),
        };
        assert_eq!(
            render_preview("rename choose → pick · fn choose · src/lib.rs:43", &prepared),
            "rename choose → pick · fn choose · src/lib.rs:43 · 2 edits · 1 file\n\
             src/lib.rs\n\
             \x20 43: pub fn choose(mode: Mode) -> usize {\n\
             \x20     pub fn pick(mode: Mode) -> usize {\n\
             \x20 62:     let offset = choose(Mode::Fast);\n\
             \x20     (the edit spans lines)\n\
             move /r/src/util.rs → /r/src/helper.rs\n\
             digest: 0123456789abcdef\n"
        );
        assert_eq!(
            render_applied("rename choose → pick · fn choose · src/lib.rs:43", &prepared),
            "applied: rename choose → pick · fn choose · src/lib.rs:43 · 2 edits · 1 file\n\
             src/lib.rs · 2 edits\n\
             moved /r/src/util.rs → /r/src/helper.rs\n"
        );
    }
}
