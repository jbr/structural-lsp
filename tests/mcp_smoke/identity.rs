//! Identity, not names (slice 2, item 5), against this repository: exact
//! `workspace/symbol` hits are followed to their definitions and deduplicated
//! by target. Gated: rust-analyzer indexes structural itself (tens of
//! seconds).

use std::path::PathBuf;

fn repo_root() -> PathBuf {
    std::fs::canonicalize(env!("CARGO_MANIFEST_DIR")).unwrap()
}

fn show(state: &structural_lsp::session::Session, symbol: &str, path: Option<&str>) -> String {
    structural_lsp::tools::Definition {
        symbol: Some(symbol.to_string()),
        path: path.map(str::to_string),
        anchor: None,
        focus: None,
        view: None,
        page: None,
        workspace: None,
        language: None,
    }
    .run(state)
    .unwrap_or_else(|error| panic!("show {symbol}: {error:#}"))
}

#[test]
#[ignore = "spawns rust-analyzer on this repository; run explicitly with -- --ignored"]
fn exact_hits_are_deduplicated_by_definition_target() {
    let state = structural_lsp::session::Session::new(repo_root());

    // `resolve` the function is reachable even though `Resolution` and
    // friends fuzzy-match it (the adapter asks for all symbols, not types
    // first), and within `src/` it is one thing.
    //
    // The scope is what keeps this about identity. `reference/` carries
    // extraction scripts, one of which defines a Python `resolve`, so the
    // unscoped query is legitimately ambiguous across two languages and
    // both servers are right to say so. Narrowing to `src/` keeps the
    // property the case exists for — several fuzzy hits that navigate to
    // one definition are one answer — while excluding a collision that
    // says nothing about it. Do not widen this back without replacing the
    // subject: the name has to be one a real tree makes common.
    let resolve = show(&state, "resolve", Some("src"));
    assert!(
        resolve.starts_with("fn resolve · src/lsp/definition.rs:"),
        "{resolve}"
    );
    assert!(resolve.contains("pub fn resolve("), "{resolve}");

    // `Server` is two things: the spike binary's enum and the smoke tests' struct.
    // Two exact hits, two distinct targets, listed with kinds and paths.
    let server = show(&state, "Server", None);
    assert!(
        server.starts_with("ambiguous: 2 definitions of 'Server'"),
        "{server}"
    );
    assert!(
        server.contains("enum Server · src/bin/spike/main.rs:"),
        "{server}"
    );
    assert!(
        server.contains("struct Server · tests/mcp_smoke/stdio_server.rs:"),
        "{server}"
    );

    // `uses` on this repository: `group_by_target` is called from the
    // shared resolution pipeline, and its unit tests are set apart.
    let uses = structural_lsp::tools::References {
        symbol: Some("group_by_target".to_string()),
        path: None,
        anchor: None,
        focus: None,
        within: None,
        page: None,
        workspace: None,
        language: None,
    }
    .run(&state)
    .unwrap_or_else(|error| panic!("uses group_by_target: {error:#}"));
    assert!(
        uses.starts_with("fn group_by_target · src/lsp/definition.rs:"),
        "{uses}"
    );
    assert!(
        uses.contains("src/tools/resolution.rs\n"),
        "the production caller's file is listed: {uses}"
    );
    assert!(
        uses.lines().any(|line| {
            line.contains(" · resolve_symbol · ") && line.contains("group_by_target(found)")
        }),
        "the enclosing function and the source line: {uses}"
    );
    assert!(uses.contains("tests (heuristic) · "), "{uses}");
    assert!(
        uses.contains("src/lsp/definition/tests.rs\n"),
        "the unit tests are set apart by their path: {uses}"
    );
    // The `use` list at the top of resolution.rs is a reference outside
    // any item: listed as `line · source` with no item segment.
    assert!(
        uses.lines().any(|line| {
            line.starts_with("  ")
                && line.contains("group_by_target")
                && line.split(" · ").count() == 2
        }),
        "{uses}"
    );

    drop(state);
}
