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

/// Read one file with hits from disk, check every hit against that text
/// when `expect` names what should sit there, and build its lines: 1-based
/// line, enclosing item, trimmed source. The bool says whether the server
/// gave an outline at all.
///
/// The file is read, not synced: syncing would make every file a broad
/// query touches an owned document for the rest of the session. The hits
/// came from the server's view; what is checked is that the disk text
/// agrees with it at every range.
pub fn lines_for(
    server: &LspServer,
    root: &Path,
    file: &FileHits,
    expect: Option<&str>,
) -> Result<(Vec<HitLine>, bool)> {
    let encoding = server.encoding();
    let aliases = server.adapter().reference_aliases();
    let text = std::fs::read_to_string(&file.path)
        .with_context(|| format!("reading {} for its references", file.path.display()))?;
    let outline = server.document_symbol(file.uri.as_str())?;
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
    Ok((lines, outline.is_some()))
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
