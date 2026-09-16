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
    let command = |label: &str| {
        format!(
            "command = [\"{}\", \"{}\", \"{}\", \"{}\"]",
            python,
            script.display(),
            log.display(),
            label
        )
    };
    std::fs::write(
        dir.join("structural.toml"),
        format!(
            "[servers.rust-analyzer]\n{}\n\n[servers.pyright]\n{}\n",
            command("rust-analyzer"),
            command("pyright")
        ),
    )
    .unwrap();
    Some(Fixture { root: dir, log })
}

impl Fixture {
    /// One pid per launch of the mock, in the order they started.
    fn launches(&self) -> Vec<u32> {
        self.events()
            .into_iter()
            .filter(|event| event.kind == "START")
            .map(|event| event.pid)
            .collect()
    }

    fn source(&self) -> PathBuf {
        self.root.join("src/lib.rs")
    }

    /// The recorded events, in the order they reached the log: each launch,
    /// and the answer to its `initialize` when it came, each naming its
    /// server.
    fn events(&self) -> Vec<Event> {
        std::fs::read_to_string(&self.log)
            .map(|text| {
                text.lines()
                    .filter_map(|line| {
                        let mut fields = line.split_whitespace();
                        Some(Event {
                            kind: fields.next()?.to_string(),
                            server: fields.next()?.to_string(),
                            pid: fields.next()?.parse().ok()?,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// One line of the mock's log: what happened, to which server, and its pid.
#[derive(Debug)]
struct Event {
    kind: String,
    server: String,
    pid: u32,
}

/// A fixture whose two servers differ: rust-analyzer's `initialize` stalls or
/// its command cannot be spawned, while pyright runs the mock normally. The
/// asymmetry is what makes independence observable.
fn asymmetric_fixture(name: &str, rust: &str) -> Option<Fixture> {
    let python = crate::test_support::python()?;
    let dir = std::env::temp_dir().join(format!("structural-host-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    let script = dir.join("mock.py");
    std::fs::write(&script, HOST_MOCK_PY).unwrap();
    std::fs::write(dir.join("src/lib.rs"), "pub fn a() {}\n").unwrap();
    std::fs::write(dir.join("src/helper.py"), "def a():\n    pass\n").unwrap();
    let log = dir.join("launches.log");
    let mock = |label: &str, stall: Option<f64>| {
        let mut argv = vec![
            format!("\"{python}\""),
            format!("\"{}\"", script.display()),
            format!("\"{}\"", log.display()),
            format!("\"{label}\""),
        ];
        if let Some(stall) = stall {
            // Quoted: `command` is a list of strings, so a bare `1.0` would
            // make the whole file unparsable and quietly leave no servers.
            argv.push(format!("\"{stall}\""));
        }
        format!("command = [{}]", argv.join(", "))
    };
    let rust = match rust {
        "mock" => mock("rust-analyzer", None),
        // Long enough that a serialized activation cannot answer the other
        // server first, and short enough not to slow the suite much.
        "stalling" => mock("rust-analyzer", Some(1.5)),
        other => format!("command = [\"{other}\"]"),
    };
    std::fs::write(
        dir.join("structural.toml"),
        format!(
            "[servers.rust-analyzer]\n{rust}\n\n[servers.pyright]\n{}\n",
            mock("pyright", None)
        ),
    )
    .unwrap();
    Some(Fixture { root: dir, log })
}

/// Wait for `n` launches to appear in the mock's log, up to a bound.
///
/// This observes a signal another thread produces — a pid that exists — and
/// never stands in for a claim that a server is ready; the bounded wait is
/// only so a failure reports rather than hangs.
fn wait_for_launches(fixture: &Fixture, n: usize) -> Vec<u32> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let launches = fixture.launches();
        if launches.len() >= n || std::time::Instant::now() > deadline {
            return launches;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// Something that panics while holding a guard must still release it, or a
/// shutdown would wait on it forever. This is the property a launch counter
/// could not give: its count came back from an ordinary call, which an unwind
/// skips past, while a guard comes back from a drop.
#[test]
fn a_panicking_participant_still_releases_its_guard() {
    let lifetime = Lifetime::new();
    let guard = lifetime.song.guard();
    let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
        let _guard = guard;
        panic!("a launcher died");
    }));
    assert!(unwound.is_err(), "the participant panicked, as arranged");

    // Read the count rather than blocking on completion: a leaked guard would
    // hang this test rather than fail it, which is the failure being guarded
    // against and not one to reproduce as a hang.
    assert_eq!(
        lifetime.song.guard_count(),
        0,
        "the guard comes back even when its holder unwinds"
    );
}

/// A shutdown arriving while a server is still handshaking leaves no child
/// behind: the one being started is killed, not published.
#[test]
fn shutdown_during_startup_leaves_no_orphan() {
    let Some(fixture) = asymmetric_fixture("orphan", "stalling") else {
        return;
    };
    let host = std::sync::Arc::new(Host::new());
    let starter = {
        let host = std::sync::Arc::clone(&host);
        let root = fixture.root.clone();
        std::thread::spawn(move || host.agent_started(AgentId::new("a"), &root))
    };

    // The stalling child exists while its launch is still inside
    // `initialize` — that instant is what this test is about.
    let launches = wait_for_launches(&fixture, 2);
    assert_eq!(launches.len(), 2, "both children were started");

    host.shutdown();

    assert!(
        launches.iter().all(|pid| !alive(*pid)),
        "no child outlives the shutdown, the mid-handshake one included: {launches:?}"
    );
    let session = starter.join().expect("the starting thread finishes");
    let workspace = session.workspace();
    assert_eq!(workspace.running().count(), 0, "nothing was published");
    assert!(
        workspace
            .slots()
            .iter()
            .all(|slot| slot.state().label() != "running"),
        "no slot claims to be running"
    );
}

/// Two servers start at once: the second launch begins before the first has
/// answered `initialize`.
/// START, READY, START, READY; overlapping launches record START, START,
/// READY, READY.
#[test]
fn servers_start_without_waiting_for_each_other() {
    let Some(fixture) = asymmetric_fixture("overlap", "stalling") else {
        return;
    };
    let host = Host::new();
    let _session = host.agent_started(AgentId::new("a"), &fixture.root);

    let events = fixture.events();
    assert_eq!(
        events.iter().filter(|event| event.kind == "START").count(),
        2,
        "both servers started: {events:?}"
    );
    let ready = |server: &str| {
        events
            .iter()
            .position(|event| event.kind == "READY" && event.server == server)
    };
    // Waiting as each launch started would answer one server's `initialize`
    // before the other was even spawned; overlapping shows the quick one
    // answering while the stalling one is still starting.
    let stalling = ready("rust-analyzer").expect("the stalling server answered initialize");
    let quick = ready("pyright").expect("the other server answered initialize");
    assert!(
        quick < stalling,
        "the second server answered `initialize` while the first was still \
         starting, so the two launches overlapped: {events:?}"
    );
    host.shutdown();
}

/// A server that cannot start is recorded as failed, says so for what it
/// serves, and does not stop the other server from serving its own language.
#[test]
fn a_server_that_will_not_start_leaves_the_others_running() {
    let Some(fixture) = asymmetric_fixture("one-fails", "no-such-server-binary") else {
        return;
    };
    let host = Host::new();
    let session = host.agent_started(AgentId::new("a"), &fixture.root);

    let workspace = session.workspace();
    let states: Vec<(&str, &str)> = workspace
        .slots()
        .iter()
        .map(|slot| (slot.name(), slot.state().label()))
        .collect();
    assert!(
        states.contains(&("pyright", "running")),
        "the server that can start did: {states:?}"
    );
    assert!(
        states.contains(&("rust-analyzer", "failed")),
        "the one that cannot is recorded as failed: {states:?}"
    );
    assert_eq!(fixture.launches().len(), 1, "only one mock was launched");

    // Reported for what it would have served, and confined to it.
    let error = workspace
        .select(Some(&fixture.root.join("src/lib.rs")))
        .unwrap_err()
        .to_string();
    assert!(error.contains("rust-analyzer"), "{error}");
    assert!(
        workspace
            .select(Some(&fixture.root.join("src/helper.py")))
            .is_ok(),
        "python still routes to the server that is running"
    );
    drop(workspace);
    host.shutdown();
}

/// A session that outlives the host finds cold slots and no servers, and says
/// so — rather than reaching for a handle whose server has been reaped.
#[test]
fn a_query_after_shutdown_reports_rather_than_panics() {
    let Some(fixture) = fixture("after-shutdown") else {
        return;
    };
    let host = Host::new();
    let session = host.agent_started(AgentId::new("a"), &fixture.root);
    assert!(!fixture.launches().is_empty(), "servers started");

    host.shutdown();

    let workspace = session.workspace();
    assert_eq!(workspace.running().count(), 0, "nothing is running");
    assert!(
        workspace
            .slots()
            .iter()
            .all(|slot| slot.state().label() == "not started"),
        "every slot is cold again"
    );
    let error = workspace.select(None).unwrap_err().to_string();
    assert!(error.contains("no language server is running"), "{error}");
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
    let seen_by_a = a.workspace().running().count();
    let seen_by_b = b.workspace().running().count();
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
        assert_eq!(remaining.running().count(), launched.len());
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
    assert_eq!(held.running().count(), launched.len());
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
    assert!(first.running().count() > 0 && second.running().count() > 0);
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
