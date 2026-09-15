use super::render::{FileGroup, HitLine, render_uses};
use super::source_line;
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
    let text = render_uses(
        "fn choose · src/lib.rs:43",
        4,
        2,
        &production,
        &tests,
        100,
        &["1 reference(s) outside the workspace root were excluded".to_string()],
    );
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
fn zero_and_one_are_worded_plainly_and_the_cap_is_a_count() {
    assert_eq!(
        render_uses("fn f · a.rs:1", 0, 0, &[], &[], 100, &[]),
        "fn f · a.rs:1 · no references\n"
    );
    let one = [FileGroup {
        path: "a.rs".into(),
        hits: vec![hit(3, Some("g"), "f()")],
    }];
    assert_eq!(
        render_uses("fn f · a.rs:1", 1, 1, &one, &[], 100, &[]),
        "fn f · a.rs:1 · 1 reference · 1 file\na.rs\n  3 · g · f()\n"
    );
    let many = [
        FileGroup {
            path: "a.rs".into(),
            hits: (1..=3).map(|line| hit(line, None, "f()")).collect(),
        },
        FileGroup {
            path: "b.rs".into(),
            hits: vec![hit(9, None, "f()")],
        },
    ];
    let capped = render_uses("fn f · a.rs:1", 5, 2, &many, &one, 2, &[]);
    assert_eq!(
        capped,
        "fn f · a.rs:1 · 5 references · 2 files\na.rs\n  1 · f()\n  2 · f()\n\
         tests (heuristic) · 1 reference\n\
         note: 3 more reference(s) not shown (listing cap 2)\n",
        "a file past the cap is not named; the count covers both sections"
    );
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
