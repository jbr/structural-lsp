//! One line of context per location, shared by `uses` and the location
//! views: the file is read from disk, each range is checked against that
//! text when a name is expected there, and every hit gets its 1-based
//! line, its enclosing item from the file's outline, and its source line.

use crate::lsp::outline;
use crate::lsp::positions::{Encoding, position_to_offset};
use crate::lsp::references::{FileHits, verify_reference_at};
use crate::lsp::server::LspServer;
use crate::tools::classify;
use crate::tools::listing::relative_display;
use anyhow::{Context, Result};
use lsp_types::Position;
use std::collections::HashSet;
use std::path::Path;

/// One hit, ready to print.
#[derive(Debug, Clone, PartialEq)]
pub struct HitLine {
    /// 1-based.
    pub line: u32,
    /// The enclosing item, when the hit is inside one.
    pub item: Option<String>,
    /// Whether the enclosing chain marks it a test (the file's path may too).
    pub is_test: bool,
    /// The trimmed source line.
    pub text: String,
}

/// The hits of one file, in line order.
#[derive(Debug, Clone, PartialEq)]
pub struct FileGroup {
    /// Root-relative.
    pub path: String,
    pub hits: Vec<HitLine>,
}

/// Why a file's hits carry no enclosing item.
pub enum OutlineGap {
    /// The server does not advertise `documentSymbol` at all.
    Unsupported,
    /// The server advertises it and would not outline this document. ty
    /// 0.0.81 answers `-32602 Document … is not open in the session`: a
    /// file this query only read is not one it opened, and the server
    /// serves outlines only for documents it was given. The reason is
    /// carried rather than flattened, because "does not support
    /// documentSymbol" would be a false claim about such a server.
    Refused(String),
}

/// Read one file with hits from disk, check every hit against that text
/// when `expect` names what should sit there, and build its lines: 1-based
/// line, enclosing item, trimmed source. The second value is present when
/// the hits carry no enclosing item, and says why.
///
/// The file is read, not synced: syncing would make every file a broad
/// query touches an owned document for the rest of the session. The hits
/// came from the server's view; what is checked is that the disk text
/// agrees with it at every range.
///
/// A refused outline degrades the answer instead of ending it. The
/// enclosing item is decoration on a hit line; the hits themselves came
/// from `references` and are verified against the disk text either way, so
/// losing the column is a note, not a failure.
pub fn lines_for(
    server: &LspServer,
    root: &Path,
    file: &FileHits,
    expect: Option<&str>,
) -> Result<(Vec<HitLine>, Option<OutlineGap>)> {
    let encoding = server.encoding();
    let aliases = server.adapter().reference_aliases();
    let text = std::fs::read_to_string(&file.path)
        .with_context(|| format!("reading {} for its references", file.path.display()))?;
    let (outline, gap) = match server.document_symbol(file.uri.as_str()) {
        Ok(Some(outline)) => (Some(outline), None),
        Ok(None) => (None, Some(OutlineGap::Unsupported)),
        Err(error) => (
            None,
            Some(OutlineGap::Refused(
                format!("{error:#}")
                    .lines()
                    .next()
                    .unwrap_or("")
                    .to_string(),
            )),
        ),
    };
    let mut lines = Vec::with_capacity(file.hits.len());
    for hit in &file.hits {
        if let Some(symbol) = expect {
            verify_reference_at(&text, hit.range, symbol, &aliases, encoding).with_context(
                || {
                    format!(
                        "{}'s reference to '{symbol}' at {}:{} does not match the file on disk; \
                     the server's view was stale — retry after it re-indexes",
                        server.name(),
                        relative_display(root, &file.path),
                        hit.range.start.line + 1
                    )
                },
            )?;
        }
        let chain = match &outline {
            Some(outline) => outline::chain_at(outline, hit.range.start).with_context(|| {
                format!(
                    "{} returned an unusable documentSymbol result",
                    server.name()
                )
            })?,
            None => Vec::new(),
        };
        lines.push(HitLine {
            line: hit.range.start.line + 1,
            item: classify::item_label(&chain),
            is_test: classify::chain_is_test(&chain),
            text: source_line(&text, hit.range.start, encoding),
        });
    }
    lines.sort_by_key(|line| line.line);
    Ok((lines, gap))
}

/// The line holding `position`, trimmed; long lines are cut with a marker.
pub fn source_line(text: &str, position: Position, encoding: Encoding) -> String {
    const MAX_CHARS: usize = 160;
    let line = position_to_offset(text, Position::new(position.line, 0), encoding)
        .ok()
        .map(|start| text[start..].lines().next().unwrap_or_default())
        .unwrap_or_default()
        .trim();
    if line.chars().count() <= MAX_CHARS {
        line.to_string()
    } else {
        let mut cut: String = line.chars().take(MAX_CHARS).collect();
        cut.push('…');
        cut
    }
}

/// Collapse rows that are the same line of the same file, returning how
/// many were removed.
///
/// A server may report several results at one location. rust-analyzer does
/// it for a derive: `#[derive(Clone, Copy, Debug, PartialEq, Eq)]` is five
/// implementations and five `textDocument/implementation` results, every
/// one pointing at the `#[derive(…)]` line, with nothing to say which
/// derived trait each is. Rendered one per result that is five identical
/// rows carrying no more information than one — and the same view rendered
/// another type's derive as a single row, so the two disagreed.
///
/// The count stays in the header, where it is the server's answer to "how
/// many", and the row that survives is the derive attribute itself, which
/// names the traits. Nothing is inferred about which result is which,
/// because the server did not say.
pub fn collapse_identical(groups: &mut [FileGroup]) -> usize {
    let mut collapsed = 0;
    for group in groups {
        let before = group.hits.len();
        let mut seen: HashSet<(u32, String)> = HashSet::new();
        group
            .hits
            .retain(|hit| seen.insert((hit.line, hit.text.clone())));
        collapsed += before - group.hits.len();
    }
    collapsed
}

/// File `lines` of the file at `path` (shown as `display`) into the
/// production and test groups of a listing: a hit is a test when its file
/// is a test file or its enclosing chain is a test module
/// (docs/resolution.md). Every listing that sets tests apart splits through
/// here, so `references` and the location views agree on what a test is.
pub fn split_tests(
    path: &Path,
    display: &str,
    lines: Vec<HitLine>,
    production: &mut Vec<FileGroup>,
    tests: &mut Vec<FileGroup>,
) {
    let path_is_test = classify::path_is_test(path);
    let (test_hits, other_hits): (Vec<HitLine>, Vec<HitLine>) = lines
        .into_iter()
        .partition(|line| path_is_test || line.is_test);
    for (bucket, hits) in [(production, other_hits), (tests, test_hits)] {
        if !hits.is_empty() {
            bucket.push(FileGroup {
                path: display.to_string(),
                hits,
            });
        }
    }
}

/// The line that opens a listing's tests section: `tests (heuristic) · 3
/// references`. `noun` is singular.
pub fn tests_heading(count: usize, noun: &str) -> String {
    format!(
        "tests (heuristic) · {count} {noun}{}\n",
        if count == 1 { "" } else { "s" }
    )
}

/// The note for a listing whose hits carry no enclosing item for `gap`'s
/// reason; without the outline, tests are known by their path alone.
pub fn gap_note(server: &str, gap: &OutlineGap) -> String {
    match gap {
        OutlineGap::Unsupported => {
            format!("{server} does not support documentSymbol; hits carry no enclosing item")
        }
        OutlineGap::Refused(why) => format!(
            "{server} would not outline every file with hits ({why}); those hits carry no \
             enclosing item"
        ),
    }
}

/// Print groups while the budget lasts; returns how many hits went unshown.
pub fn render_groups(text: &mut String, groups: &[FileGroup], budget: &mut usize) -> usize {
    let mut omitted = 0;
    for group in groups {
        if *budget == 0 {
            omitted += group.hits.len();
            continue;
        }
        text.push_str(&group.path);
        text.push('\n');
        for hit in &group.hits {
            if *budget == 0 {
                omitted += 1;
                continue;
            }
            *budget -= 1;
            match &hit.item {
                Some(item) => text.push_str(&format!("  {} · {item} · {}\n", hit.line, hit.text)),
                None => text.push_str(&format!("  {} · {}\n", hit.line, hit.text)),
            }
        }
    }
    omitted
}

#[cfg(test)]
mod tests;
