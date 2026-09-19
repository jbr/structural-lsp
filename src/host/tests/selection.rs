//! What selection starts, asserted against the process table.
//!
//! `discovery::selection` already tests that a plan omits a server; these
//! test that no process exists for it. The distinction is the point of the
//! milestone — "irrelevant servers do not start" is a claim about pids,
//! and it is only checkable because the mock records every launch
//! (`backlog/discovery/selection.md`).

use super::fixtures::{selective_fixture, servers_launched};
use crate::host::{AgentId, Host};

#[test]
fn a_server_no_evidence_calls_for_is_never_launched() {
    // The defect this milestone closes. Before plans drove the slots, both
    // configured servers were warmed at `agent_started` whatever the tree
    // held, so a Python server indexed a project with no Python in it.
    //
    // This is the process-table assertion the plan asked for: not that the
    // plan value omits pyright, which `discovery::selection` already
    // tests, but that no pyright process exists
    // (`backlog/discovery/selection.md`).
    let Some(rust_only) = selective_fixture(
        "rust-only",
        &[("Cargo.toml", ""), ("src/lib.rs", "pub fn a() {}\n")],
        "",
    ) else {
        return;
    };
    let host = Host::new();
    let _session = host.agent_started(AgentId::new("a"), &rust_only.root);
    assert_eq!(
        servers_launched(&rust_only),
        ["rust-analyzer"],
        "a tree with no Python gets no Python server"
    );
    host.shutdown();
}

#[test]
fn both_are_launched_when_the_tree_calls_for_both() {
    // The control for the case above: without it, "pyright did not start"
    // would also pass if pyright could never start at all, and the
    // assertion would be measuring the mock rather than the selection.
    let Some(polyglot) = selective_fixture(
        "polyglot",
        &[
            ("Cargo.toml", ""),
            ("src/lib.rs", "pub fn a() {}\n"),
            ("src/helper.py", "def a():\n    pass\n"),
        ],
        "",
    ) else {
        return;
    };
    let host = Host::new();
    let _session = host.agent_started(AgentId::new("a"), &polyglot.root);
    assert_eq!(servers_launched(&polyglot), ["pyright", "rust-analyzer"]);
    host.shutdown();
}

#[test]
fn a_disabled_server_does_not_start_where_its_language_is() {
    // Overrides win, asserted against the process table rather than the
    // plan value. The tree has Python in it, so evidence alone would warm
    // pyright; `disabled = true` is the capability configuration did not
    // have before the catalogue, and this is a project saying "not here".
    let Some(disabled) = selective_fixture(
        "disabled",
        &[
            ("Cargo.toml", ""),
            ("src/lib.rs", "pub fn a() {}\n"),
            ("src/helper.py", "def a():\n    pass\n"),
        ],
        "disabled = true\n",
    ) else {
        return;
    };
    let host = Host::new();
    let _session = host.agent_started(AgentId::new("a"), &disabled.root);
    assert_eq!(
        servers_launched(&disabled),
        ["rust-analyzer"],
        "the tree has Python and the project said not here"
    );
    host.shutdown();
}

