use super::*;

fn entry(name: &str, location: &str, sites: &[(u32, &str)]) -> Entry {
    Entry {
        kind: "fn".to_string(),
        name: name.to_string(),
        location: location.to_string(),
        sites: sites
            .iter()
            .map(|(line, text)| Site {
                line: *line,
                text: Some(text.to_string()),
            })
            .collect(),
    }
}

fn calls(incoming: bool, project: Vec<Entry>, tests: Vec<Entry>, outside: Vec<Entry>) -> Calls<'static> {
    Calls {
        head: "fn project_turns · src/lib.rs:19",
        incoming,
        total: project.len() + tests.len() + outside.len(),
        project,
        tests,
        outside,
        notes: Vec::new(),
    }
}

const TOOLCHAIN: &str = "/home/u/.rustup/toolchains/nightly/lib/rustlib/src/rust/library";

/// The shape measured on rust-analyzer: project callees first in full,
/// the standard library's as one line each (in the order the body first
/// calls them, which `calls.rs` sorts), with their toolchain paths left
/// out.
#[test]
fn outside_callees_are_one_line_each_after_the_project() {
    let outside = vec![
        entry("lock", &format!("{TOOLCHAIN}/std/src/sync/poison/mutex.rs:490"), &[(20, "…")]),
        entry(
            "unwrap_or",
            &format!("{TOOLCHAIN}/core/src/option.rs:1038"),
            &[(21, "let first = guard.first().copied().unwrap_or(0);"), (24, "…")],
        ),
        entry("len", &format!("{TOOLCHAIN}/alloc/src/vec/mod.rs:3144"), &[(25, "…")]),
    ];
    let project = vec![
        entry("probe", "src/lib.rs:10", &[(23, "let probed = LiveProbe.probe();")]),
        entry("helper", "src/lib.rs:15", &[(22, "let total = helper(first);")]),
    ];
    assert_eq!(
        render_calls(calls(false, project, Vec::new(), outside)),
        "fn project_turns · src/lib.rs:19 · 5 callees\n\
         fn probe · src/lib.rs:10\n\
         \x20 23 · let probed = LiveProbe.probe();\n\
         fn helper · src/lib.rs:15\n\
         \x20 22 · let total = helper(first);\n\
         outside the workspace · 3 callees\n\
         fn lock · called at 20\n\
         fn unwrap_or · called at 21, 24\n\
         fn len · called at 25\n"
    );
}

/// Test callers are set apart under the heading `references` uses; a
/// caller outside the root keeps its path and sites, which are in its own
/// file.
#[test]
fn test_and_outside_callers_have_their_own_sections() {
    let project = vec![entry("caller", "src/lib.rs:28", &[(29, "helper(2)")])];
    let tests = vec![
        entry("helper_adds", "src/lib.rs:45", &[(46, "assert_eq!(helper(1), 2);")]),
        entry("integration_helper", "tests/integration.rs:4", &[(5, "assert_eq!(helper(3), 4);")]),
    ];
    let outside = vec![entry("sibling", "/w/other/src/lib.rs:3", &[(4, "helper(9)")])];
    assert_eq!(
        render_calls(calls(true, project, tests, outside)),
        "fn project_turns · src/lib.rs:19 · 4 callers\n\
         fn caller · src/lib.rs:28\n\
         \x20 29 · helper(2)\n\
         tests (heuristic) · 2 callers\n\
         fn helper_adds · src/lib.rs:45\n\
         \x20 46 · assert_eq!(helper(1), 2);\n\
         fn integration_helper · tests/integration.rs:4\n\
         \x20 5 · assert_eq!(helper(3), 4);\n\
         outside the workspace · 1 caller\n\
         fn sibling · /w/other/src/lib.rs:3\n\
         \x20 4 · helper(9)\n"
    );
}

/// pyright reports each overload of `print` as its own callee; without
/// their locations the rows are the same, so they are one row that says
/// how many definitions it stands for, and the header keeps the count.
#[test]
fn outside_rows_that_print_the_same_are_one_row() {
    let outside = vec![
        entry("len", "/t/builtins.pyi:1535", &[(9, "…")]),
        entry("print", "/t/builtins.pyi:1774", &[(11, "…")]),
        entry("print", "/t/builtins.pyi:1782", &[(11, "…")]),
    ];
    assert_eq!(
        render_calls(calls(false, Vec::new(), Vec::new(), outside)),
        "fn project_turns · src/lib.rs:19 · 3 callees\n\
         outside the workspace · 3 callees\n\
         fn len · called at 9\n\
         fn print · called at 11 · 2 definitions\n"
    );
}

#[test]
fn no_calls_is_one_line() {
    assert_eq!(
        render_calls(calls(true, Vec::new(), Vec::new(), Vec::new())),
        "fn project_turns · src/lib.rs:19 · no callers\n"
    );
}

/// The cap counts lines across every section; the entries it never reached
/// are counted, and a listing that fits exactly is not called cut.
#[test]
fn the_cap_counts_what_it_left_out() {
    let project = vec![
        entry("a", "src/a.rs:1", &[(2, "a()")]),
        entry("b", "src/b.rs:1", &[(3, "b()")]),
    ];
    let outside = vec![entry("len", "/t/vec.rs:1", &[(4, "…")])];
    let text = render_capped(calls(false, project, Vec::new(), outside), 3);
    assert_eq!(
        text,
        "fn project_turns · src/lib.rs:19 · 3 callees\n\
         fn a · src/a.rs:1\n\
         \x20 2 · a()\n\
         fn b · src/b.rs:1\n\
         note: listing cut at 3 lines; 1 more callee not shown\n"
    );
    let exact = vec![entry("a", "src/a.rs:1", &[(2, "a()")])];
    let text = render_capped(calls(false, exact, Vec::new(), Vec::new()), 2);
    assert!(!text.contains("note:"), "{text}");
}
