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


#[test]
fn sibling_members_of_one_workspace_share_one_launch_and_keep_their_own_view() {
    // The sharing bug this milestone's second half closes. `agent_started`
    // keyed on the working directory verbatim, so two agents in two
    // members of one cargo workspace got two workspaces and two servers —
    // and each of those servers indexed the whole workspace anyway
    // (`reference/server-root-discovery.md`). Keying on the outermost
    // manifest in the span makes them one.
    //
    // The other half of the claim is that sharing widens without answers
    // widening: each session still views its own member, which is what
    // `path`, the filters and every rendered location are relative to.
    let Some(fixture) = selective_fixture(
        "siblings",
        &[
            ("Cargo.toml", "[workspace]\n"),
            ("crates/alpha/Cargo.toml", ""),
            ("crates/alpha/src/lib.rs", "pub fn a() {}\n"),
            ("crates/beta/Cargo.toml", ""),
            ("crates/beta/src/lib.rs", "pub fn b() {}\n"),
        ],
        "",
    ) else {
        return;
    };
    let root = std::fs::canonicalize(&fixture.root).unwrap();
    let alpha_dir = root.join("crates/alpha");
    let beta_dir = root.join("crates/beta");

    let host = Host::new();
    let alpha = host.agent_started(AgentId::new("alpha"), &alpha_dir);
    let beta = host.agent_started(AgentId::new("beta"), &beta_dir);

    assert_eq!(
        servers_launched(&fixture),
        ["rust-analyzer"],
        "one server for the workspace, not one per member"
    );
    assert_eq!(
        alpha.workspace().root(),
        root,
        "both are keyed on the workspace, not on the member they started in"
    );
    assert_eq!(beta.workspace().root(), root);

    assert_eq!(alpha.view(), alpha_dir, "each still asks about its own member");
    assert_eq!(beta.view(), beta_dir);

    host.shutdown();
}

#[test]
fn a_member_with_no_workspace_above_it_is_its_own_key() {
    // The control: sharing follows the project, not the directory depth.
    // Two unrelated projects in one repository key apart, which is why the
    // VCS root bounds the search rather than being the answer.
    let Some(fixture) = selective_fixture(
        "unrelated",
        &[
            ("backend/Cargo.toml", ""),
            ("backend/src/lib.rs", "pub fn a() {}\n"),
            ("tools/Cargo.toml", ""),
            ("tools/src/lib.rs", "pub fn b() {}\n"),
        ],
        "",
    ) else {
        return;
    };
    let root = std::fs::canonicalize(&fixture.root).unwrap();
    // A repository holding both, and no manifest of its own.
    std::fs::create_dir_all(root.join(".git")).unwrap();
    // Each project root is its own workspace, and configuration is read
    // from the project root — so the fixture's own `structural.toml`, at
    // the repository, is read by neither. Writing one per project is the
    // consequence of the key moving up, made concrete.
    fixture.write_config(&root.join("backend"));
    fixture.write_config(&root.join("tools"));

    let host = Host::new();
    let backend = host.agent_started(AgentId::new("backend"), &root.join("backend"));
    let tools = host.agent_started(AgentId::new("tools"), &root.join("tools"));

    assert_eq!(backend.workspace().root(), root.join("backend"));
    assert_eq!(
        tools.workspace().root(),
        root.join("tools"),
        "keying on the repository would fuse two unrelated projects"
    );
    assert_eq!(
        servers_launched(&fixture),
        ["rust-analyzer", "rust-analyzer"],
        "two projects, two servers — the sharing above is not simply \
         everything collapsing onto one key"
    );

    host.shutdown();
}
