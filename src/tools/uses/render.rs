//! Rendering for `uses`: the header with its counts, one line per file,
//! one line per hit, the tests section, and the notes.
//!
//! Every byte is paid for on every call: the path appears once per file,
//! each hit is `line · item · source`, and notes appear only when something
//! is degraded.

use crate::tools::hits::{FileGroup, render_groups};

/// `header · N references · M files`, the production groups, then the
/// tests section, then notes. Past `cap` hits, the rest is a count.
pub(super) fn render_uses(
    header: &str,
    total: usize,
    files: usize,
    production: &[FileGroup],
    tests: &[FileGroup],
    cap: usize,
    notes: &[String],
) -> String {
    let mut text = match total {
        0 => format!("{header} · no references\n"),
        1 => format!("{header} · 1 reference · 1 file\n"),
        _ => format!(
            "{header} · {total} references · {files} file{}\n",
            if files == 1 { "" } else { "s" }
        ),
    };
    let mut budget = cap;
    let production_omitted = render_groups(&mut text, production, &mut budget);
    let test_total: usize = tests.iter().map(|group| group.hits.len()).sum();
    let mut tests_omitted = 0;
    if test_total > 0 {
        text.push_str(&format!(
            "tests (heuristic) · {test_total} reference{}\n",
            if test_total == 1 { "" } else { "s" }
        ));
        tests_omitted = render_groups(&mut text, tests, &mut budget);
    }
    let omitted = production_omitted + tests_omitted;
    if omitted > 0 {
        text.push_str(&format!(
            "note: {omitted} more reference(s) not shown (listing cap {cap})\n"
        ));
    }
    for note in notes {
        text.push_str(&format!("note: {note}\n"));
    }
    text
}
