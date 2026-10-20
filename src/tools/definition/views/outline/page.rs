//! An outline as numbered pages of lines (docs/tools.md).
//!
//! An outline is a tree flattened to lines, each with its depth. A page is
//! [`PAGE_LINES`] of them, counted from 1. A page that starts inside a
//! subtree is preceded by the ancestors of its first line, which an earlier
//! page showed, so its indentation still reads as nesting; those context
//! lines are not counted against the page.

use super::super::MAX_LINES;
use anyhow::{Result, bail};

/// Lines per page.
pub(super) const PAGE_LINES: usize = MAX_LINES;

/// One outline line: its text without indentation, and its depth.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Line {
    pub depth: usize,
    pub text: String,
}

/// The lines page `number` shows, with the ancestors that give its first
/// line its place.
#[derive(Debug)]
pub(super) struct Page<'a> {
    pub number: usize,
    pub context: Vec<&'a Line>,
    pub lines: &'a [Line],
    /// Whether any line of `all` follows this page.
    pub more: bool,
}

/// How many pages `total` lines fill; an empty outline is one page.
pub(super) fn count(total: usize) -> usize {
    total.div_ceil(PAGE_LINES).max(1)
}

/// The first line index page `number` shows, refusing `0`.
pub(super) fn start_of(number: usize) -> Result<usize> {
    if number == 0 {
        bail!("`page` counts from 1");
    }
    Ok((number - 1) * PAGE_LINES)
}

/// Page `number` of `all`. `unfetched` says lines exist past `all` that the
/// caller has not produced yet (a directory outline outlines its files only
/// as far as the page it was asked for). A page past the end is an argument
/// error that names the last page.
pub(super) fn page(all: &[Line], number: usize, unfetched: bool) -> Result<Page<'_>> {
    let start = start_of(number)?;
    if start > 0 && start >= all.len() {
        bail!(
            "`page: {number}` is past the end: this outline has {} page(s)",
            count(all.len())
        );
    }
    let end = (start + PAGE_LINES).min(all.len());
    Ok(Page {
        number,
        context: ancestors(all, start),
        lines: &all[start..end],
        more: end < all.len() || unfetched,
    })
}

/// The lines enclosing `all[at]`, outermost first: for each shallower
/// depth, the nearest earlier line at it.
fn ancestors(all: &[Line], at: usize) -> Vec<&Line> {
    let Some(first) = all.get(at) else {
        return Vec::new();
    };
    let mut wanted = first.depth;
    let mut found = Vec::new();
    for line in all[..at].iter().rev() {
        if wanted == 0 {
            break;
        }
        if line.depth < wanted {
            found.push(line);
            wanted = line.depth;
        }
    }
    found.reverse();
    found
}

impl Page<'_> {
    /// The context lines, then the page's own, two spaces per depth.
    pub fn render(&self, text: &mut String) {
        for line in self.context.iter().copied().chain(self.lines) {
            text.push_str(&"  ".repeat(line.depth));
            text.push_str(&line.text);
            text.push('\n');
        }
    }

    /// The header's suffix past page 1: ` · page 2 of 3`, or ` · page 2`
    /// when the total is not known.
    pub fn label(&self, total: Option<usize>) -> String {
        match (self.number, total) {
            (1, _) => String::new(),
            (number, Some(total)) => format!(" · page {number} of {total}"),
            (number, None) => format!(" · page {number}"),
        }
    }

    /// The continuation note: what is left, and the argument that shows it.
    pub fn continuation(&self, left: &str) -> String {
        if !self.more {
            return String::new();
        }
        format!("note: {left}; `page: {}` continues\n", self.number + 1)
    }
}
