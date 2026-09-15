//! Rendering for `uses`: the header with its counts, one line per file,
//! one line per hit, the tests section, and the notes.
//!
//! Every byte is paid for on every call: the path appears once per file,
//! each hit is `line · item · source`, and notes appear only when something
//! is degraded.

/// One hit, ready to print.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct HitLine {
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
pub(super) struct FileGroup {
    /// Root-relative.
    pub path: String,
    pub hits: Vec<HitLine>,
}

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

/// Print groups while the budget lasts; returns how many hits went unshown.
fn render_groups(text: &mut String, groups: &[FileGroup], budget: &mut usize) -> usize {
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
