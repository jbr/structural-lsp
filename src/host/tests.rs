//! The host against the scripted mock: one launch shared by every agent bound
//! to a workspace, a release that leaves another agent working, workspaces
//! that do not block one another, and a shutdown that leaves no child behind.
//!
//! The mock replaces both built-in servers through `structural.toml`, so no
//! real language server is needed and every launch is recorded.

use super::*;
use crate::test_support::HOST_MOCK_PY;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// A workspace on disk whose built-in servers are overridden to run the mock.
struct Fixture {
    root: PathBuf,
    log: PathBuf,
}

/// `None` when there is no usable Python, which every test here reports as a
/// skip. Nothing else about the fixture can fail.
fn fixture(name: &str) -> Option<Fixture> {
    let python = crate::test_support::python()?;
    let dir = std::env::temp_dir().join(format!("structural-host-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    let script = dir.join("mock.py");
    std::fs::write(&script, HOST_MOCK_PY).unwrap();
    // A source file, so file-scoped routing has something to match.
    std::fs::write(dir.join("src/lib.rs"), "pub fn a() {}\n").unwrap();
    let log = dir.join("launches.log");
    let command = format!(
        "command = [\"{}\", \"{}\", \"{}\"]",
        python,
        script.display(),
        log.display()
    );
    std::fs::write(
        dir.join("structural.toml"),
        format!("[servers.rust-analyzer]\n{command}\n\n[servers.pyright]\n{command}\n"),
    )
    .unwrap();
    Some(Fixture { root: dir, log })
}

impl Fixture {
    /// One pid per launch of the mock, in the order they started.
    fn launches(&self) -> Vec<u32> {
        std::fs::read_to_string(&self.log)
            .map(|text| {
                text.lines()
                    .filter_map(|line| line.strip_prefix("START ")?.trim().parse().ok())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn source(&self) -> PathBuf {
        self.root.join("src/lib.rs")
    }
}

/// Whether `pid` names a live process. `ps` is the portable-enough probe:
/// teardown waits on the child, so a reaped pid is gone rather than a zombie.
fn alive(pid: u32) -> bool {
    Command::new("ps")
        .args(["-p", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

macro_rules! fixture {
    ($name:literal) => {
        match fixture($name) {
            Some(fixture) => fixture,
            None => {
                eprintln!("skipping: python3 not available");
                return;
            }
        }
    };
}

#[test]
fn two_agents_on_one_root_share_one_launch() {
    let workspace = fixture!("share");
    let host = Host::new();

    let a = host.agent_started(AgentId::new("a"), &workspace.root);
    let after_first = workspace.launches();
    assert!(
        !after_first.is_empty(),
        "acquiring a workspace starts its servers"
    );

    let b = host.agent_started(AgentId::new("b"), &workspace.root);
    assert_eq!(
        workspace.launches().len(),
        after_first.len(),
        "the second agent shares the first agent's servers"
    );
    // Read one at a time: both agents hold the same workspace's lock, so two
    // guards here would deadlock rather than disagree.
    let seen_by_a = a.workspace().servers().len();
    let seen_by_b = b.workspace().servers().len();
    assert_eq!(seen_by_a, seen_by_b, "both agents see the same workspace");
    host.shutdown();
}

#[test]
fn releasing_one_agent_leaves_another_working() {
    let workspace = fixture!("release");
    let host = Host::new();
    let a = host.agent_started(AgentId::new("a"), &workspace.root);
    let b = host.agent_started(AgentId::new("b"), &workspace.root);
    let launched = workspace.launches();

    host.agent_ended(&AgentId::new("a"));

    let after = workspace.launches();
    assert_eq!(
        after.len(),
        launched.len(),
        "releasing one agent does not disturb the shared servers"
    );
    // Scoped: shutdown takes the workspace lock, so no guard may outlive it.
    {
        let remaining = b.workspace();
        assert_eq!(remaining.servers().len(), launched.len());
        assert!(
            !remaining
                .select(Some(&workspace.source()))
                .unwrap()
                .is_empty(),
            "the remaining agent still routes a .rs file to a server"
        );
    }
    drop(a);
    host.shutdown();
}

#[test]
fn shutdown_reaps_a_workspace_a_released_agent_still_holds() {
    let workspace = fixture!("detached-reap");
    let host = Host::new();
    let session = host.agent_started(AgentId::new("a"), &workspace.root);
    let pids = workspace.launches();
    assert!(!pids.is_empty(), "the workspace started its servers");

    // Release the agent but keep the session: the entry leaves the map while
    // its servers stay up, so shutdown must still find them. Without that,
    // these children would live until the session happened to drop.
    host.agent_ended(&AgentId::new("a"));
    assert!(
        pids.iter().copied().all(alive),
        "a released workspace a session still holds keeps running"
    );

    host.shutdown();

    assert!(
        pids.iter().copied().all(|pid| !alive(pid)),
        "shutdown reaches a workspace the map no longer names: {pids:?}"
    );
    drop(session);
}

#[test]
fn a_broken_config_file_is_reported_not_fatal() {
    let dir =
        std::env::temp_dir().join(format!("structural-host-badconfig-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // A present but unusable file: `cmd` is not a key any server entry takes.
    std::fs::write(
        dir.join("structural.toml"),
        "[servers.thing]\ncmd = [\"x\"]\n",
    )
    .unwrap();

    let host = Host::new();
    let session = host.agent_started(AgentId::new("a"), &dir);

    // The workspace still binds — the MCP surface must come up — and every
    // call says why it cannot answer.
    let error = session.workspace().select(None).unwrap_err().to_string();
    assert!(error.contains("structural.toml"), "{error}");
    assert!(error.contains("cmd"), "{error}");
    host.shutdown();
}

#[test]
fn a_released_agent_leaves_a_live_session_its_servers() {
    let workspace = fixture!("outlive");
    let host = Host::new();
    let session = host.agent_started(AgentId::new("a"), &workspace.root);
    let launched = workspace.launches();

    host.agent_ended(&AgentId::new("a"));

    // The binding, not the agent list, is what keeps a workspace alive while
    // one caller still holds it: a missed or early agent-end must not cut an
    // operation short. (When residency should end after the last agent is the
    // lifecycle policy of unit 2.)
    let held = session.workspace();
    assert_eq!(held.servers().len(), launched.len());
    assert!(!held.select(Some(&workspace.source())).unwrap().is_empty());
    drop(held);
    drop(session);
    host.shutdown();
}

#[test]
fn independent_workspaces_hold_their_workspaces_at_once() {
    let one = fixture!("independent-one");
    let two = fixture!("independent-two");
    let host = Host::new();
    let a = host.agent_started(AgentId::new("a"), &one.root);
    let b = host.agent_started(AgentId::new("b"), &two.root);

    // Both guards are held at once because an operation takes only its own
    // workspace's lock: two workspaces' calls do not serialize against each
    // other. This is deliberately *not* a concurrent-acquisition test — the
    // host does hold the map lock across startup, so a second `agent_started`
    // queues; see the locking rule in `docs/host-extraction.md`.

    let first = a.workspace();
    let second = b.workspace();
    assert_ne!(one.root, two.root);
    assert!(!first.servers().is_empty() && !second.servers().is_empty());
    assert_ne!(
        one.launches(),
        two.launches(),
        "each workspace launched its own servers"
    );
    drop(first);
    drop(second);
    host.shutdown();
}

#[test]
fn shutdown_reaps_every_workspace_child() {
    let workspace = fixture!("reap");
    let host = Host::new();
    let session = host.agent_started(AgentId::new("a"), &workspace.root);

    let pids = workspace.launches();
    assert!(!pids.is_empty(), "the workspace started its servers");
    assert!(
        pids.iter().copied().all(alive),
        "the mock servers are running before shutdown"
    );

    host.shutdown();

    assert!(
        pids.iter().copied().all(|pid| !alive(pid)),
        "shutdown left no child behind: {pids:?}"
    );
    drop(session);
}
