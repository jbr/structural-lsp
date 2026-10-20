use super::render::{Listing, render_references};
use crate::tools::hits::{FileGroup, HitLine, source_line};
use crate::lsp::positions::Encoding;
use lsp_types::Position;

fn hit(line: u32, item: Option<&str>, text: &str) -> HitLine {
    HitLine {
        line,
        item: item.map(str::to_string),
        is_test: false,
        text: text.to_string(),
    }
}

/// A listing with no scope and no offset, capped at `cap`.
fn listing<'a>(
    header: &'a str,
    total: usize,
    files: usize,
    production: &'a [FileGroup],
    tests: &'a [FileGroup],
    cap: usize,
    notes: &'a [String],
) -> Listing<'a> {
    Listing {
        header,
        total,
        files,
        within: None,
        production,
        tests,
        page: 1,
        cap,
        notes,
    }
}

fn group(path: &str, lines: std::ops::RangeInclusive<u32>) -> FileGroup {
    FileGroup {
        path: path.into(),
        hits: lines.map(|line| hit(line, None, "f()")).collect(),
    }
}

#[test]
fn the_listing_is_byte_exact() {
    let production = [
        FileGroup {
            path: "src/lib.rs".into(),
            hits: vec![
                hit(58, Some("run"), "let offset = choose(Mode::Fast);"),
                hit(74, None, "pub use super::choose;"),
            ],
        },
        FileGroup {
            path: "src/other.rs".into(),
            hits: vec![hit(12, Some("Other::go"), "choose(mode)")],
        },
    ];
    let tests = [FileGroup {
        path: "src/lib.rs".into(),
        hits: vec![hit(66, Some("tests::smoke"), "assert_eq!(choose(Mode::Fast), 1);")],
    }];
    let notes = ["1 reference(s) outside the workspace root were excluded".to_string()];
    let text = render_references(&listing(
        "fn choose · src/lib.rs:43",
        4,
        2,
        &production,
        &tests,
        100,
        &notes,
    ));
    assert_eq!(
        text,
        "fn choose · src/lib.rs:43 · 4 references · 2 files\n\
         src/lib.rs\n  58 · run · let offset = choose(Mode::Fast);\n  74 · pub use super::choose;\n\
         src/other.rs\n  12 · Other::go · choose(mode)\n\
         tests (heuristic) · 1 reference\n\
         src/lib.rs\n  66 · tests::smoke · assert_eq!(choose(Mode::Fast), 1);\n\
         note: 1 reference(s) outside the workspace root were excluded\n"
    );
}

#[test]
fn zero_and_one_are_worded_plainly() {
    assert_eq!(
        render_references(&listing("fn f · a.rs:1", 0, 0, &[], &[], 100, &[])),
        "fn f · a.rs:1 · no references\n"
    );
    let one = [FileGroup {
        path: "a.rs".into(),
        hits: vec![hit(3, Some("g"), "f()")],
    }];
    assert_eq!(
        render_references(&listing("fn f · a.rs:1", 1, 1, &one, &[], 100, &[])),
        "fn f · a.rs:1 · 1 reference · 1 file\na.rs\n  3 · g · f()\n"
    );
}

#[test]
fn a_capped_listing_says_where_the_rest_is_and_how_to_continue() {
    let production = [
        group("a.rs", 1..=3),
        group("src/b.rs", 9..=9),
        group("src/c.rs", 1..=2),
        group("src/deep/d.rs", 4..=4),
    ];
    let tests = [group("tests/t.rs", 5..=5)];
    let capped = render_references(&listing("fn f · a.rs:1", 8, 5, &production, &tests, 2, &[]));
    assert_eq!(
        capped,
        "fn f · a.rs:1 · 8 references · 5 files · page 1 of 4\n\
         a.rs\n  1 · f()\n  2 · f()\n\
         tests (heuristic) · 1 reference\n\
         not shown · 6 references in 5 files:\n\
         \x20 src/ · 3 in 2 files\n\
         \x20 ./ · 1 in 1 file\n\
         \x20 src/deep/ · 1 in 1 file\n\
         \x20 tests/ · 1 in 1 file\n\
         note: `page: 2` continues, or narrow with `within` naming a directory above\n",
        "the remainder is by directory, largest first, ties in path order"
    );

    // The last page crosses from production into tests as one sequence.
    let last = render_references(&Listing {
        page: 3,
        cap: 3,
        ..listing("fn f · a.rs:1", 8, 5, &production, &tests, 2, &[])
    });
    assert_eq!(
        last,
        "fn f · a.rs:1 · 8 references · 5 files · page 3 of 3\n\
         src/deep/d.rs\n  4 · f()\n\
         tests (heuristic) · 1 reference\n\
         tests/t.rs\n  5 · f()\n"
    );
}

#[test]
fn a_scoped_listing_counts_against_the_whole() {
    let inside = [group("src/b.rs", 9..=9)];
    let scoped = render_references(&Listing {
        within: Some(("src/", 8)),
        ..listing("fn f · a.rs:1", 1, 1, &inside, &[], 100, &[])
    });
    assert_eq!(
        scoped,
        "fn f · a.rs:1 · 1 of 8 references · 1 file within src/\nsrc/b.rs\n  9 · f()\n"
    );
    let empty = render_references(&Listing {
        within: Some(("docs/", 8)),
        ..listing("fn f · a.rs:1", 0, 0, &[], &[], 100, &[])
    });
    assert_eq!(empty, "fn f · a.rs:1 · no references within docs/ (8 outside it)\n");
}

#[test]
fn many_directories_fold_past_the_summary_cap() {
    let production: Vec<FileGroup> = (0..15)
        .map(|index| group(&format!("d{index:02}/x.rs"), 1..=1))
        .collect();
    let text = render_references(&listing("fn f · a.rs:1", 15, 15, &production, &[], 1, &[]));
    assert!(text.contains("  d12/ · 1 in 1 file\n"), "{text}");
    assert!(!text.contains("d13/"), "{text}");
    assert!(text.contains("  … 2 more directories · 2 in all\n"), "{text}");
}

#[test]
fn source_lines_are_trimmed_and_long_ones_cut() {
    let text = "first\n    let x = choose(1);   \nlast";
    assert_eq!(
        source_line(text, Position::new(1, 12), Encoding::Utf8),
        "let x = choose(1);"
    );
    assert_eq!(source_line(text, Position::new(2, 0), Encoding::Utf8), "last");
    assert_eq!(source_line(text, Position::new(9, 0), Encoding::Utf8), "");
    let long = "x".repeat(200);
    let cut = source_line(&long, Position::new(0, 0), Encoding::Utf8);
    assert_eq!(cut.chars().count(), 161);
    assert!(cut.ends_with('…'));
}
