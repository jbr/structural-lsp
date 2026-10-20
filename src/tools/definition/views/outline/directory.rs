//! A directory's outline: every file under it a running server serves, in
//! path order, each followed by its top-level items (docs/tools.md).
//!
//! Bounded the way a file outline is, by pages of lines. Files are
//! outlined only as far as the page asked for, so a later page re-reads
//! the files before it (outlines are of the disk now, never remembered),
//! and the files no running server can outline are counted on page 1
//! rather than skipped in silence.

use super::page::{self, Line, PAGE_LINES};
use super::{file_nodes, symbol_line};
use crate::discovery::evidence::{FILE_BUDGET, skipped};
use crate::lsp::adapter::language_for;
use crate::tools::listing::relative_display;
use crate::workspace::Workspace;
use anyhow::Result;
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

/// Where one file's outline comes from.
enum Route {
    /// The running server in this slot.
    Served(usize),
    /// No slot's languages cover the file's extension.
    Unserved,
    /// Slots serve it, and none is running: their names and states.
    Down(String),
}

pub(super) fn outline(
    state: &mut Workspace,
    root: &Path,
    directory: &Path,
    number: usize,
) -> Result<String> {
    let (files, truncated) = walk(directory);
    let mut served: Vec<(PathBuf, usize)> = Vec::new();
    let mut unserved: BTreeMap<String, usize> = BTreeMap::new();
    let mut down: BTreeMap<(String, String), usize> = BTreeMap::new();
    for file in files {
        match route(state, &file) {
            Route::Served(index) => served.push((file, index)),
            Route::Unserved => *unserved.entry(extension(&file)).or_default() += 1,
            Route::Down(servers) => *down.entry((extension(&file), servers)).or_default() += 1,
        }
    }

    // Outline file by file until the page is full and one line past it is
    // known, so the page can say whether anything follows.
    let wanted = page::start_of(number)? + PAGE_LINES;
    let mut lines: Vec<Line> = Vec::new();
    let mut ends: Vec<usize> = Vec::new();
    let mut readied: HashSet<usize> = HashSet::new();
    for (file, index) in &served {
        if lines.len() > wanted {
            break;
        }
        let server = state.server_mut(*index)?;
        if readied.insert(*index) {
            server.sync_owned()?;
            server.wait_ready()?;
        }
        let display = relative_display(root, file);
        match file_nodes(server, file)? {
            None => lines.push(Line {
                depth: 0,
                text: format!("{display} · outline not supported by {}", server.name()),
            }),
            Some(nodes) if nodes.is_empty() => lines.push(Line {
                depth: 0,
                text: format!("{display} · empty outline"),
            }),
            Some(nodes) => {
                lines.push(Line {
                    depth: 0,
                    text: display,
                });
                lines.extend(nodes.iter().map(|node| Line {
                    depth: 1,
                    text: symbol_line(server, node),
                }));
            }
        }
        ends.push(lines.len());
    }
    let unfetched = ends.len() < served.len();
    let page = page::page(&lines, number, unfetched)?;

    let shown_to = page::start_of(number)? + page.lines.len();
    let complete = ends.iter().filter(|&&end| end <= shown_to).count();
    let named = relative_display(root, directory);
    let named = if named.is_empty() { "." } else { &named };
    let total = (!unfetched).then(|| page::count(lines.len()));
    let mut text = if served.is_empty() {
        format!("{named} · no file here has a running server to outline it\n")
    } else {
        format!(
            "{named} · {} file{}{}\n",
            served.len(),
            if served.len() == 1 { "" } else { "s" },
            page.label(total)
        )
    };
    page.render(&mut text);
    if number == 1 {
        text.push_str(&left_out(&unserved, &down, truncated, named));
    }
    text.push_str(&page.continuation(&format!(
        "{} more file(s)",
        served.len() - complete
    )));
    Ok(text)
}

/// Which running server outlines `file`, by its extension alone — a file
/// with no extension is no language's, rather than every server's.
fn route(state: &Workspace, file: &Path) -> Route {
    let serving: Vec<usize> = state
        .slots()
        .iter()
        .enumerate()
        .filter(|(_, slot)| language_for(&slot.adapter().languages(), file).is_some())
        .map(|(index, _)| index)
        .collect();
    if serving.is_empty() {
        return Route::Unserved;
    }
    if let Some(&index) = serving.iter().find(|&&index| state.server(index).is_ok()) {
        return Route::Served(index);
    }
    let slots = state.slots();
    Route::Down(
        serving
            .iter()
            .map(|&index| format!("{} {}", slots[index].name(), slots[index].state().label()))
            .collect::<Vec<_>>()
            .join(", "),
    )
}

fn extension(file: &Path) -> String {
    file.extension()
        .map(|extension| format!("`{}`", extension.to_string_lossy()))
        .unwrap_or_else(|| "no extension".to_string())
}

/// What page 1 says about the files it could not outline: one line per
/// reason, each capped.
fn left_out(
    unserved: &BTreeMap<String, usize>,
    down: &BTreeMap<(String, String), usize>,
    truncated: bool,
    named: &str,
) -> String {
    let mut text = String::new();
    if !unserved.is_empty() {
        let total: usize = unserved.values().sum();
        let mut kinds: Vec<String> = unserved
            .iter()
            .take(5)
            .map(|(extension, count)| format!("{extension} ({count})"))
            .collect();
        if unserved.len() > 5 {
            kinds.push(format!("and {} more", unserved.len() - 5));
        }
        text.push_str(&format!(
            "note: {total} file(s) no server here serves are not listed: {}\n",
            kinds.join(", ")
        ));
    }
    for ((extension, servers), count) in down {
        text.push_str(&format!(
            "note: {count} {extension} file(s) are not listed: {servers}\n"
        ));
    }
    if truncated {
        text.push_str(&format!(
            "note: the walk stopped at {FILE_BUDGET} files; the rest of `{named}` is not listed\n"
        ));
    }
    text
}

/// Every file under `directory`, sorted by path, not descending into
/// skipped or symlinked directories (the evidence scan's rule), and whether
/// the walk stopped at [`FILE_BUDGET`]. Sorting the path text puts
/// `definition.rs` before `definition/render.rs`: a module file before the
/// directory of its children.
fn walk(directory: &Path) -> (Vec<PathBuf>, bool) {
    let mut files = Vec::new();
    let mut truncated = false;
    let mut pending = vec![directory.to_path_buf()];
    'walk: while let Some(next) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&next) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                if !kind.is_symlink() && !skipped(&entry.file_name().to_string_lossy()) {
                    pending.push(entry.path());
                }
            } else if kind.is_file() {
                if files.len() >= FILE_BUDGET {
                    truncated = true;
                    break 'walk;
                }
                files.push(entry.path());
            }
        }
    }
    files.sort_by(|a, b| a.as_os_str().cmp(b.as_os_str()));
    (files, truncated)
}
