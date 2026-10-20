//! Rendering for the call views: the header with the count, the
//! workspace's entries, then the tests section and the outside section
//! when they have entries, then notes (docs/tools.md).
//!
//! An entry is `kind name · path:line` with its call sites under it,
//! `line · source`. A callee outside the workspace root is one line,
//! `kind name · called at 12, 40`: its sites are lines of the target, whose
//! body is the thing asked about, and its location is a path into a
//! toolchain or a dependency that would cost more bytes than the rest of
//! the row. A caller outside the root keeps the full shape, because its
//! call sites are in its own file and the path is how to reach them.

use crate::tools::hits::tests_heading;
use crate::tools::listing::header;

/// One call site.
pub(super) struct Site {
    /// 1-based.
    pub line: u32,
    /// The trimmed source line, when the file holding it could be read.
    pub text: Option<String>,
}

/// One caller or callee.
pub(super) struct Entry {
    pub kind: String,
    pub name: String,
    /// `path:line`, root-relative when inside the root.
    pub location: String,
    /// In source order.
    pub sites: Vec<Site>,
}

/// One call view's answer, sorted into its sections.
pub(super) struct Calls<'a> {
    pub head: &'a str,
    pub incoming: bool,
    pub total: usize,
    /// Each section in the order it is printed: the workspace's entries
    /// and the tests in path order, outside callees in the order the
    /// target's body first calls each.
    pub project: Vec<Entry>,
    pub tests: Vec<Entry>,
    pub outside: Vec<Entry>,
    pub notes: Vec<String>,
}

/// Lines printed before the rest is counted.
const CAP: usize = super::super::MAX_LINES;

pub(super) fn render_calls(calls: Calls<'_>) -> String {
    render_capped(calls, CAP)
}

fn render_capped(calls: Calls<'_>, cap: usize) -> String {
    let noun = if calls.incoming { "caller" } else { "callee" };
    if calls.total == 0 {
        return format!("{} · no {noun}s\n", calls.head);
    }
    let mut text = format!(
        "{} · {} {noun}{}\n",
        calls.head,
        calls.total,
        plural(calls.total)
    );
    let mut lines = Lines {
        text: &mut text,
        budget: cap,
        omitted: 0,
        cut: false,
    };
    lines.full(&calls.project);
    if !calls.tests.is_empty() {
        lines.heading(&tests_heading(calls.tests.len(), noun));
        lines.full(&calls.tests);
    }
    if !calls.outside.is_empty() {
        let count = calls.outside.len();
        let heading = format!("outside the workspace · {count} {noun}{}\n", plural(count));
        lines.heading(&heading);
        if calls.incoming {
            lines.full(&calls.outside);
        } else {
            lines.compact(&calls.outside);
        }
    }
    let (cut, omitted) = (lines.cut, lines.omitted);
    if cut {
        let rest = if omitted > 0 {
            format!("; {omitted} more {noun}{} not shown", plural(omitted))
        } else {
            String::new()
        };
        text.push_str(&format!("note: listing cut at {cap} lines{rest}\n"));
    }
    for note in &calls.notes {
        text.push_str(&format!("note: {note}\n"));
    }
    text
}

/// The listing under its cap: every printed line spends one, and an entry
/// reached after the cap is spent is counted rather than printed.
struct Lines<'a> {
    text: &'a mut String,
    budget: usize,
    /// Entries not printed at all.
    omitted: usize,
    /// Whether any line was refused, a call site included.
    cut: bool,
}

impl Lines<'_> {
    /// A section heading, free of the cap; once the cap is spent it is
    /// left out, and the section's rows are counted as omitted.
    fn heading(&mut self, heading: &str) {
        if self.budget > 0 {
            self.text.push_str(heading);
        }
    }

    fn line(&mut self, line: &str) -> bool {
        if self.budget == 0 {
            self.cut = true;
            return false;
        }
        self.budget -= 1;
        self.text.push_str(line);
        self.text.push('\n');
        true
    }

    /// Each entry's line, then its call sites under it.
    fn full(&mut self, entries: &[Entry]) {
        for entry in entries {
            let shown = self.line(&header(Some(&entry.kind), &entry.name, &entry.location));
            if !shown {
                self.omitted += 1;
                continue;
            }
            for site in &entry.sites {
                let Some(source) = &site.text else {
                    continue;
                };
                self.line(&format!("  {} · {source}", site.line));
            }
        }
    }

    /// One line per entry: its name and the lines that call it.
    ///
    /// Without its location, an entry can print the same as another: an
    /// overloaded function is one declaration per overload (pyright
    /// reports both of `print`'s), each called at the same lines. Such
    /// rows are one row naming how many definitions it stands for, as a
    /// derive's implementations are one row (docs/tools.md); the header
    /// keeps the server's count.
    fn compact(&mut self, entries: &[Entry]) {
        let mut rows: Vec<(String, usize)> = Vec::new();
        for entry in entries {
            let lines: Vec<String> = entry
                .sites
                .iter()
                .map(|site| site.line.to_string())
                .collect();
            let called = format!("called at {}", lines.join(", "));
            let row = header(Some(&entry.kind), &entry.name, &called);
            match rows.iter_mut().find(|(seen, _)| *seen == row) {
                Some((_, count)) => *count += 1,
                None => rows.push((row, 1)),
            }
        }
        for (row, count) in rows {
            let row = if count > 1 {
                format!("{row} · {count} definitions")
            } else {
                row
            };
            if !self.line(&row) {
                self.omitted += count;
            }
        }
    }
}

fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

#[cfg(test)]
mod tests;
