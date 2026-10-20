use super::*;

fn hit(line: u32, text: &str) -> HitLine {
    HitLine {
        line,
        item: Some("ToolOutcome".to_string()),
        is_test: false,
        text: text.to_string(),
    }
}

/// rust-analyzer's shape: five derived traits, five results, one line.
#[test]
fn identical_rows_of_one_line_become_one() {
    let derive = "#[derive(Clone, Copy, Debug, PartialEq, Eq)]";
    let mut groups = vec![FileGroup {
        path: "src/log.rs".to_string(),
        hits: vec![hit(940, derive), hit(940, derive), hit(940, derive)],
    }];
    assert_eq!(collapse_identical(&mut groups), 2);
    assert_eq!(groups[0].hits, vec![hit(940, derive)]);
}

/// Real implementations at their own lines are untouched, and so is a
/// derive beside them — the case that already rendered one row, which
/// is why the two disagreed.
#[test]
fn distinct_lines_are_kept() {
    let mut groups = vec![FileGroup {
        path: "src/log.rs".to_string(),
        hits: vec![
            hit(940, "#[derive(Debug)]"),
            hit(950, "impl Display for Notify {"),
            hit(970, "impl Clone for Notify {"),
        ],
    }];
    assert_eq!(collapse_identical(&mut groups), 0);
    assert_eq!(groups[0].hits.len(), 3);
}

/// The same text on different lines is two hits, not a duplicate.
#[test]
fn the_same_text_at_another_line_is_not_a_duplicate() {
    let mut groups = vec![FileGroup {
        path: "src/log.rs".to_string(),
        hits: vec![hit(10, "#[derive(Debug)]"), hit(20, "#[derive(Debug)]")],
    }];
    assert_eq!(collapse_identical(&mut groups), 0);
}

/// Files are collapsed independently, and the count is the total.
#[test]
fn each_file_collapses_on_its_own() {
    let mut groups = vec![
        FileGroup {
            path: "a.rs".to_string(),
            hits: vec![hit(1, "#[derive(Debug)]"), hit(1, "#[derive(Debug)]")],
        },
        FileGroup {
            path: "b.rs".to_string(),
            hits: vec![hit(1, "#[derive(Debug)]")],
        },
    ];
    assert_eq!(collapse_identical(&mut groups), 1);
    assert_eq!(groups[0].hits.len(), 1);
    assert_eq!(groups[1].hits.len(), 1);
}

/// A hit is a test by its file's path or by its own chain; the rest of the
/// file stays production, so one file can feed both sections.
#[test]
fn tests_split_by_path_or_by_chain() {
    let mut production = Vec::new();
    let mut tests = Vec::new();
    let mut in_module = hit(30, "impl Probe for Table {");
    in_module.is_test = true;
    split_tests(
        Path::new("/r/src/lib.rs"),
        "src/lib.rs",
        vec![hit(9, "impl Probe for LiveProbe {"), in_module.clone()],
        &mut production,
        &mut tests,
    );
    split_tests(
        Path::new("/r/tests/probe.rs"),
        "tests/probe.rs",
        vec![hit(4, "impl Probe for Fake {")],
        &mut production,
        &mut tests,
    );
    assert_eq!(
        production,
        vec![FileGroup {
            path: "src/lib.rs".to_string(),
            hits: vec![hit(9, "impl Probe for LiveProbe {")],
        }]
    );
    let paths: Vec<&str> = tests.iter().map(|group| group.path.as_str()).collect();
    assert_eq!(paths, ["src/lib.rs", "tests/probe.rs"]);
    assert_eq!(
        tests_heading(2, "implementation"),
        "tests (heuristic) · 2 implementations\n"
    );
}
