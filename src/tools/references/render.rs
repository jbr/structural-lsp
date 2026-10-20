//! Rendering for `references`: the header with its counts, one line per
//! file, one line per hit, the tests section, what a capped listing left
//! out, and the notes.
//!
//! Every byte is paid for on every call: the path appears once per file,
//! each hit is `line · item · source`, and notes appear only when something
//! is degraded. A listing past its cap does not stop at a count: the
//! remainder is summarized by directory, largest first, so the caller sees
//! where the rest is and can ask for it with `within` or `page`
//! (docs/tools.md).

use crate::tools::hits::{FileGroup, HitLine, render_groups, tests_heading};
use std::collections::BTreeMap;

/// Directories listed in a remainder summary before the rest is a count.
const MAX_REMAINDER_DIRECTORIES: usize = 12;

/// What one `references` answer holds, before the window is applied.
pub(super) struct Listing<'a> {
    pub header: &'a str,
    /// References and files in the listing, after `within`.
    pub total: usize,
    pub files: usize,
    /// The `within` scope as displayed, with the unscoped total, when one
    /// was given.
    pub within: Option<(&'a str, usize)>,
    /// In path order; production first, then tests.
    pub production: &'a [FileGroup],
    pub tests: &'a [FileGroup],
    /// Which window of `cap` references, counting from 1.
    pub page: usize,
    /// References per page.
    pub cap: usize,
    pub notes: &'a [String],
}

/// The header, the hits in the window, a summary of what follows it, then
/// notes.
pub(super) fn render_references(listing: &Listing<'_>) -> String {
    let offset = (listing.page - 1) * listing.cap;
    let mut skip = offset;
    let mut budget = listing.cap;
    let (production, production_rest) = window(listing.production, &mut skip, &mut budget);
    let (tests, tests_rest) = window(listing.tests, &mut skip, &mut budget);
    let rest: Vec<(&str, usize)> = production_rest.into_iter().chain(tests_rest).collect();
    let pages = listing.total.div_ceil(listing.cap).max(1);

    let mut text = header_line(listing, pages);
    let mut unlimited = usize::MAX;
    render_groups(&mut text, &production, &mut unlimited);
    let test_total: usize = listing.tests.iter().map(|group| group.hits.len()).sum();
    if test_total > 0 {
        text.push_str(&tests_heading(test_total, "reference"));
        render_groups(&mut text, &tests, &mut unlimited);
    }
    if !rest.is_empty() {
        render_remainder(&mut text, &rest);
        text.push_str(&format!(
            "note: `page: {}` continues, or narrow with `within` naming a directory above\n",
            listing.page + 1
        ));
    }
    for note in listing.notes {
        text.push_str(&format!("note: {note}\n"));
    }
    text
}

/// `header · N references · M files`, scoped as asked, with the page when
/// there is more than one.
fn header_line(listing: &Listing<'_>, pages: usize) -> String {
    let Listing {
        header,
        total,
        files,
        within,
        page,
        ..
    } = *listing;
    let counts = match (total, within) {
        (0, None) | (0, Some((_, 0))) => "no references".to_string(),
        (0, Some((scope, overall))) => {
            format!("no references within {scope} ({overall} outside it)")
        }
        (total, Some((scope, overall))) => format!(
            "{total} of {overall} references · {files} file{} within {scope}",
            plural(files)
        ),
        (1, None) => "1 reference · 1 file".to_string(),
        (total, None) => format!("{total} references · {files} file{}", plural(files)),
    };
    let window = if pages > 1 {
        format!(" · page {page} of {pages}")
    } else {
        String::new()
    };
    format!("{header} · {counts}{window}\n")
}

/// The part of `groups` inside the window, and the hits each file has past
/// it. `skip` and `budget` carry across calls, so production and tests are
/// one sequence.
fn window<'a>(
    groups: &'a [FileGroup],
    skip: &mut usize,
    budget: &mut usize,
) -> (Vec<FileGroup>, Vec<(&'a str, usize)>) {
    let mut shown = Vec::new();
    let mut rest = Vec::new();
    for group in groups {
        let skipped = (*skip).min(group.hits.len());
        *skip -= skipped;
        let remaining: &[HitLine] = &group.hits[skipped..];
        let taken = (*budget).min(remaining.len());
        *budget -= taken;
        if taken > 0 {
            shown.push(FileGroup {
                path: group.path.clone(),
                hits: remaining[..taken].to_vec(),
            });
        }
        if remaining.len() > taken {
            rest.push((group.path.as_str(), remaining.len() - taken));
        }
    }
    (shown, rest)
}

/// What follows the window, by the directory each file is in: one line per
/// directory, the largest first, so the place to narrow to is the first
/// thing read.
fn render_remainder(text: &mut String, rest: &[(&str, usize)]) {
    let mut directories: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for (path, count) in rest {
        let directory = match path.rfind('/') {
            Some(slash) => path[..=slash].to_string(),
            None => "./".to_string(),
        };
        let entry = directories.entry(directory).or_default();
        entry.0 += count;
        entry.1 += 1;
    }
    let references: usize = rest.iter().map(|(_, count)| count).sum();
    text.push_str(&format!(
        "not shown · {references} reference{} in {} file{}:\n",
        plural(references),
        rest.len(),
        plural(rest.len())
    ));
    let mut ranked: Vec<(String, (usize, usize))> = directories.into_iter().collect();
    // Largest first; the map's order breaks ties, so equal counts stay in
    // path order.
    ranked.sort_by_key(|(_, (count, _))| std::cmp::Reverse(*count));
    for (directory, (count, files)) in ranked.iter().take(MAX_REMAINDER_DIRECTORIES) {
        text.push_str(&format!(
            "  {directory} · {count} in {files} file{}\n",
            plural(*files)
        ));
    }
    if ranked.len() > MAX_REMAINDER_DIRECTORIES {
        let unlisted = &ranked[MAX_REMAINDER_DIRECTORIES..];
        let count: usize = unlisted.iter().map(|(_, (count, _))| count).sum();
        text.push_str(&format!(
            "  … {} more director{} · {count} in all\n",
            unlisted.len(),
            if unlisted.len() == 1 { "y" } else { "ies" }
        ));
    }
}

fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}
