//! What selection starts, asserted against the process table.
//!
//! `discovery::selection` already tests that a plan omits a server; these
//! test that no process exists for it. The distinction is the point of the
//! milestone — "irrelevant servers do not start" is a claim about pids,
//! and it is only checkable because the mock records every launch
//! (`backlog/discovery/selection.md`).

use super::bound;
use super::fixtures::{ladder_fixture, launch_order, selective_fixture, servers_launched};
use crate::host::Host;

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
    let _session = bound(&host, "a", &rust_only.root);
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
    let _session = bound(&host, "a", &polyglot.root);
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
    let _session = bound(&host, "a", &disabled.root);
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
    let alpha = bound(&host, "alpha", &alpha_dir);
    let beta = bound(&host, "beta", &beta_dir);

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

    assert_eq!(
        alpha.view(),
        alpha_dir,
        "each still asks about its own member"
    );
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
    let backend = bound(&host, "backend", &root.join("backend"));
    let tools = bound(&host, "tools", &root.join("tools"));

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

/// The fallback clause, asserted against the launch log: a preferred
/// candidate that spawns and then fails its handshake is stepped over, and
/// the next candidate on the ladder is tried — in order, and only then.
///
/// `dies` rather than a missing binary on purpose. Availability is
/// **successful initialization**, not an executable on `PATH`
/// (`docs/discovery.md`), and only a candidate that got as far as a pid can
/// show the log that both were attempted and in which order.
#[test]
fn a_candidate_that_fails_its_handshake_is_stepped_over_in_order() {
    let Some(fixture) = ladder_fixture("ladder-dies", "dies", "serves") else {
        return;
    };
    let host = Host::new();
    let session = bound(&host, "a", &fixture.root);

    assert_eq!(
        launch_order(&fixture),
        ["a-preferred", "b-alternative"],
        "the preferred candidate was tried first and the alternative second"
    );

    let workspace = session.workspace();
    let slot = &workspace.slots()[0];
    assert_eq!(
        workspace.slots().len(),
        1,
        "one plan, one slot — the alternative is a rung, not a second server"
    );
    assert_eq!(slot.state().label(), "running", "{:?}", slot.state());
    assert_eq!(
        slot.name(),
        "b-alternative",
        "the slot stands for the candidate that actually started"
    );

    // Retained, never swallowed: a fallback that hides the reason it fell
    // back is worse than no fallback (`backlog/discovery/selection.md`).
    let attempts = slot.failed_attempts();
    assert_eq!(attempts.len(), 1, "{attempts:?}");
    assert_eq!(attempts[0].server, "a-preferred");
    assert!(!attempts[0].reason.is_empty(), "{attempts:?}");

    // And reported: the footer is the only place a *successful* fallback
    // is visible, since every other report is about something being wrong.
    let descriptions = workspace.descriptions();
    assert!(
        descriptions.contains("a-preferred failed to start"),
        "{descriptions}"
    );

    drop(workspace);
    host.shutdown();
}

/// The same ladder when the preferred candidate cannot be spawned at all.
/// No pid for it, so the log names only the alternative — which is why the
/// retained attempt, not the log, is what says the first rung was tried.
#[test]
fn a_candidate_that_cannot_be_spawned_falls_back_too() {
    let Some(fixture) = ladder_fixture("ladder-missing", "missing", "serves") else {
        return;
    };
    let host = Host::new();
    let session = bound(&host, "a", &fixture.root);

    assert_eq!(
        launch_order(&fixture),
        ["b-alternative"],
        "the preferred candidate never became a process"
    );
    let workspace = session.workspace();
    let slot = &workspace.slots()[0];
    assert_eq!(slot.name(), "b-alternative");
    assert_eq!(
        slot.failed_attempts()
            .iter()
            .map(|attempt| attempt.server.clone())
            .collect::<Vec<_>>(),
        ["a-preferred"],
    );

    drop(workspace);
    host.shutdown();
}

/// The control. Without it, "the alternative started" would also pass if
/// the ladder were walked to the bottom every time, and the assertion would
/// be measuring the fixture rather than the fallback.
#[test]
fn an_alternative_is_never_started_behind_a_candidate_that_works() {
    let Some(fixture) = ladder_fixture("ladder-control", "serves", "serves") else {
        return;
    };
    let host = Host::new();
    let session = bound(&host, "a", &fixture.root);

    assert_eq!(
        launch_order(&fixture),
        ["a-preferred"],
        "the ladder stops at the first candidate that initializes"
    );
    let workspace = session.workspace();
    let slot = &workspace.slots()[0];
    assert_eq!(slot.name(), "a-preferred");
    assert!(slot.failed_attempts().is_empty());
    assert!(
        !workspace.descriptions().contains("failed to start"),
        "nothing fell back, so the footer says nothing about it"
    );

    drop(workspace);
    host.shutdown();
}

/// A ladder that runs out is **bounded by the list**: every candidate was
/// tried once, the slot failed, and the failure names each one rather than
/// only the last. Nothing retries and no timer decides anything.
#[test]
fn a_ladder_that_runs_out_reports_every_candidate_it_tried() {
    let Some(fixture) = ladder_fixture("ladder-exhausted", "missing", "missing") else {
        return;
    };
    let host = Host::new();
    let session = bound(&host, "a", &fixture.root);

    assert!(
        launch_order(&fixture).is_empty(),
        "neither candidate was a real command"
    );
    let workspace = session.workspace();
    let slot = &workspace.slots()[0];
    assert_eq!(slot.state().label(), "failed", "{:?}", slot.state());
    let reason = slot.state().reason().unwrap_or_default().to_string();
    assert!(reason.contains("a-preferred"), "{reason}");
    assert!(
        reason.contains("b-alternative"),
        "the last candidate's failure is not the whole story: {reason}"
    );

    // And the caller is told the same thing, with each candidate's own
    // install hint beside it.
    let error = format!(
        "{:#}",
        workspace
            .select(Some(&fixture.root.join("src/lib.rs")))
            .expect_err("nothing on the ladder started")
    );
    assert!(error.contains("a-preferred"), "{error}");
    assert!(error.contains("b-alternative"), "{error}");

    drop(workspace);
    host.shutdown();
}
