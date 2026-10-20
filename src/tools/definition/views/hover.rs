//! `view = "type"`: `textDocument/hover` at the target, reduced to its code
//! blocks (the signature or type). With an anchor the hover is over the
//! focus itself, no definition step: rust-analyzer answers with the type
//! of the expression the focus spans; other servers answer at its start.

use super::unsupported;
use crate::tools::listing::relative_display;
use crate::tools::resolution::anchored::{Anchored, Focus, locate_focus};
use crate::tools::resolution::{Resolved, Target, resolve_target, server_named, sync_target};
use crate::workspace::Workspace;
use anyhow::{Context, Result, bail};
use lsp_types::Range;
use serde_json::Value;
use std::path::Path;

pub(super) fn run(state: &mut Workspace, root: &Path, target: Target<'_>) -> Result<String> {
    if let Some(anchor) = target.anchor {
        if target.symbol.is_some() {
            bail!("give either `symbol` or `anchor`, not both");
        }
        let scope = target
            .path
            .map(|path| {
                root.join(path)
                    .canonicalize()
                    .with_context(|| format!("`{path}` does not exist under the workspace root"))
            })
            .transpose()?;
        let Focus {
            server: index,
            file,
            uri,
            range,
            focus_text,
            line,
            ..
        } = locate_focus(
            state,
            root,
            Anchored {
                scope,
                anchor,
                focus: target.focus,
                language: target.language,
            },
        )?;
        let server = state.server(index)?;
        if !server.supports("hoverProvider") {
            return Ok(unsupported("type", server.name()));
        }
        let mut head = format!(
            "type of `{focus_text}` · {}:{line}",
            relative_display(root, &file)
        );
        let wide = range.start != range.end
            && !server
                .adapter()
                .hover_params(&uri, range)
                .get("position")
                .is_some_and(|position| position.get("start").is_some());
        if wide
            && focus_text
                .chars()
                .any(|c| !(c.is_alphanumeric() || c == '_'))
        {
            head.push_str(&format!(
                " (at its start: {} hovers a position, not a range)",
                server.name()
            ));
        }
        let raw = server.hover(&uri, range)?;
        return Ok(format!("{head}\n{}", render(&raw)));
    }
    let found = match resolve_target(state, target)? {
        Resolved::Listed(listing) => return Ok(listing),
        Resolved::One(found) => *found,
    };
    let head = super::target_header(state, root, &found)?;
    let (_, synced) = sync_target(state, root, &found)?;
    let server = server_named(state, &found.identity.server)?;
    if !server.supports("hoverProvider") {
        return Ok(unsupported("type", server.name()));
    }
    server.wait_ready()?;
    let start = found.target.selection_range.start;
    let raw = server.hover(&synced.uri, Range { start, end: start })?;
    Ok(format!("{head}\n{}", render(&raw)))
}

/// The hover's code blocks, one after another; with none, its first
/// paragraph; with nothing, said.
fn render(raw: &Value) -> String {
    let Some(contents) = raw.get("contents") else {
        return "no hover information\n".to_string();
    };
    let (blocks, prose) = extract(contents);
    if !blocks.is_empty() {
        return blocks.join("\n") + "\n";
    }
    match prose.into_iter().find(|p| !p.trim().is_empty()) {
        Some(paragraph) => paragraph.trim().to_string() + "\n",
        None => "no hover information\n".to_string(),
    }
}

/// Code blocks and prose paragraphs from every `Hover.contents` shape:
/// `MarkupContent`, a `MarkedString`, or an array of them.
fn extract(contents: &Value) -> (Vec<String>, Vec<String>) {
    let mut blocks = Vec::new();
    let mut prose = Vec::new();
    match contents {
        Value::String(text) => split_markdown(text, &mut blocks, &mut prose),
        Value::Array(items) => {
            for item in items {
                let (b, p) = extract(item);
                blocks.extend(b);
                prose.extend(p);
            }
        }
        Value::Object(object) => {
            if let Some(code) = object
                .get("language")
                .and(object.get("value"))
                .and_then(Value::as_str)
            {
                blocks.push(code.trim().to_string());
            } else if let Some(text) = object.get("value").and_then(Value::as_str) {
                // Plain text has no fences, so the same split just finds
                // its paragraphs.
                split_markdown(text, &mut blocks, &mut prose);
            }
        }
        _ => {}
    }
    (blocks, prose)
}

/// Fenced code blocks out of markdown; the rest as paragraphs.
fn split_markdown(text: &str, blocks: &mut Vec<String>, prose: &mut Vec<String>) {
    let mut in_block = false;
    let mut block = String::new();
    let mut paragraph = String::new();
    for line in text.lines() {
        if line.trim_start().starts_with("```") {
            if in_block {
                blocks.push(block.trim_end().to_string());
                block.clear();
            }
            in_block = !in_block;
            continue;
        }
        if in_block {
            block.push_str(line);
            block.push('\n');
        } else if line.trim().is_empty() || line.trim() == "---" {
            if !paragraph.trim().is_empty() {
                prose.push(paragraph.clone());
            }
            paragraph.clear();
        } else {
            paragraph.push_str(line);
            paragraph.push('\n');
        }
    }
    if in_block && !block.trim().is_empty() {
        blocks.push(block.trim_end().to_string());
    }
    if !paragraph.trim().is_empty() {
        prose.push(paragraph);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn code_blocks_win_over_prose_and_every_shape_is_read() {
        let markdown = json!({ "contents": { "kind": "markdown", "value": "```rust\nfixture\n```\n\n```rust\npub fn choose(mode: Mode) -> usize\n```\n\n---\n\nPicks a number.\n" } });
        assert_eq!(
            render(&markdown),
            "fixture\npub fn choose(mode: Mode) -> usize\n"
        );
        let marked = json!({ "contents": [{ "language": "python", "value": "(function) def choose(flag: bool) -> int" }, "Picks."] });
        assert_eq!(
            render(&marked),
            "(function) def choose(flag: bool) -> int\n"
        );
        let prose_only =
            json!({ "contents": { "kind": "plaintext", "value": "Just words.\n\nMore." } });
        assert_eq!(render(&prose_only), "Just words.\n");
        assert_eq!(render(&Value::Null), "no hover information\n");
        assert_eq!(render(&json!({ "contents": "" })), "no hover information\n");
    }
}
