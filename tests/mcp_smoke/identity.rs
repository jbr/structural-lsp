//! Identity, not names (slice 2, item 5), against this repository: exact
//! `workspace/symbol` hits are followed to their definitions and deduplicated
//! by target. Gated: rust-analyzer indexes structural itself (tens of
//! seconds).

use mcplease::traits::Tool;
use std::path::PathBuf;

fn repo_root() -> PathBuf {
    std::fs::canonicalize(env!("CARGO_MANIFEST_DIR")).unwrap()
}

fn show(state: &mut structural::state::State, symbol: &str) -> String {
    structural::tools::Show {
        symbol: symbol.to_string(),
        path: None,
    }
    .execute(state, &mcplease::types::RequestContext::default())
    .unwrap_or_else(|error| panic!("show {symbol}: {error:#}"))
}

#[test]
#[ignore = "spawns rust-analyzer on this repository; run explicitly with -- --ignored"]
fn exact_hits_are_deduplicated_by_definition_target() {
    let mut state = structural::state::State::new(repo_root());

    // `resolve` the function is reachable even though `Resolution` and
    // friends fuzzy-match it (the adapter asks for all symbols, not types
    // first), and it is one thing.
    let resolve = show(&mut state, "resolve");
    assert!(
        resolve.starts_with("fn resolve · src/lsp/definition.rs:"),
        "{resolve}"
    );
    assert!(resolve.contains("pub fn resolve("), "{resolve}");

    // `Server` is two things: the spike binary's enum and the smoke tests' struct.
    // Two exact hits, two distinct targets, listed with kinds and paths.
    let server = show(&mut state, "Server");
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
    let uses = structural::tools::Uses {
        symbol: "group_by_target".to_string(),
        path: None,
    }
    .execute(&mut state, &mcplease::types::RequestContext::default())
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
        uses.contains(" · resolve_symbol · let mut identities = group_by_target(found);\n"),
        "the enclosing function and the source line: {uses}"
    );
    assert!(uses.contains("tests (heuristic) · "), "{uses}");
    assert!(
        uses.contains("src/lsp/definition/tests.rs\n"),
        "the unit tests are set apart by their path: {uses}"
    );
    // The `use` line at the top of resolve.rs is a reference outside any
    // item: listed without one.
    assert!(
        uses.contains(" · use crate::lsp::definition::{\n")
            || uses.contains(" · Candidate, DefinitionSet, DefinitionTarget, Identity, Partition, Resolution, group_by_target,\n"),
        "{uses}"
    );

    drop(state);
}
