//! Shutdown admission is tested separately from frontend signal delivery.
use super::*;
use std::sync::mpsc;
use std::time::Duration;

#[test]
fn soft_shutdown_protects_an_admitted_operation_and_refuses_new_calls() {
    let fixture = fixture!("soft-operation");
    let host = std::sync::Arc::new(Host::new());
    let session = bound(&host, "a", &fixture.root);
    let pids = fixture.launches();
    let operation = session.acquire(None, None).unwrap();
    host.request_shutdown();
    assert!(session.acquire(None, None).is_err());
    assert!(pids.iter().copied().all(alive));
    assert!(host.lifetime.song.guard_count() > 0);
    let (done, completed) = mpsc::channel();
    let waiter = {
        let host = std::sync::Arc::clone(&host);
        std::thread::spawn(move || {
            host.wait_shutdown();
            done.send(()).unwrap();
        })
    };
    // The held operation is a structural barrier, not a timing assumption.
    assert!(completed.try_recv().is_err());
    drop(operation);
    completed.recv_timeout(Duration::from_secs(10)).unwrap();
    waiter.join().unwrap();
    assert!(pids.iter().copied().all(|pid| !alive(pid)));
}

#[test]
fn completion_covers_admission_before_any_launch_election() {
    let host = std::sync::Arc::new(Host::new());
    // Exactly the token `start` acquires before touching the slot's election.
    let admission = host.lifetime.admit().unwrap();
    host.request_shutdown();
    assert!(host.lifetime.admit().is_err());
    assert_eq!(host.lifetime.song.guard_count(), 1);
    let (done, completed) = mpsc::channel();
    let waiter = {
        let host = std::sync::Arc::clone(&host);
        std::thread::spawn(move || {
            host.wait_shutdown();
            done.send(()).unwrap();
        })
    };
    assert!(completed.try_recv().is_err());
    drop(admission);
    completed.recv_timeout(Duration::from_secs(5)).unwrap();
    waiter.join().unwrap();
}

#[test]
fn force_reaches_running_processes_while_workspace_is_held() {
    let fixture = fixture!("force-held-operation");
    let host = std::sync::Arc::new(Host::new());
    let session = bound(&host, "a", &fixture.root);
    let pids = fixture.launches();
    let operation = session.acquire(None, None).unwrap();
    host.request_shutdown();
    let (done, completed) = mpsc::channel();
    let waiter = {
        let host = std::sync::Arc::clone(&host);
        std::thread::spawn(move || {
            host.wait_shutdown();
            done.send(()).unwrap();
        })
    };
    host.force_shutdown();
    host.force_shutdown();
    // A request made through the still-held workspace fails without requiring
    // that workspace lock to be released for force to reach the owner.
    for server in operation.running() {
        assert!(
            server
                .request("test/after-force", serde_json::json!({}))
                .is_err()
        );
    }
    drop(operation);
    completed.recv_timeout(Duration::from_secs(10)).unwrap();
    waiter.join().unwrap();
    host.wait_shutdown();
    assert!(pids.iter().copied().all(|pid| !alive(pid)));
}

/// How long a test waits on another thread before calling it a deadlock.
/// Generous, because the machine is shared: nothing here measures it.
const DEADLOCK: Duration = Duration::from_secs(30);

/// A launcher waiting on the workspace lock when shutdown is claimed stops
/// its own child instead of publishing it, and the sweep does not start
/// until it has — so the sweep's `Cold` is the last write to the slot
/// (`docs/shutdown.md`).
///
/// The interleaving is picked by locks and the mock's log, not by time.
/// The test holds the workspace without admission, as a raw guard does,
/// and demands a server that is not running; the launch elected for it
/// handshakes and warms its index, and the mock records `SYMBOLS` under
/// the new pid when the warm's last request arrives. From there the
/// launcher has only local work and the lock between it and publication,
/// and the test is holding the lock. A second admission, the test's own,
/// keeps the sweep back after the launcher returns, so the slot can be
/// read as the launcher left it before the sweep overwrites it.
#[test]
fn a_launcher_parked_on_the_workspace_lock_stops_its_child_before_the_sweep() {
    let fixture = fixture!("parked-launcher");
    fixture.write_config_with_modes(&fixture.root, None, Some("indexes"));
    let host = std::sync::Arc::new(Host::new());
    let session = bound(&host, "a", &fixture.root);
    let before = fixture.launches();
    assert_eq!(before.len(), 2, "both servers started: {before:?}");

    let mut held = session.workspace();
    let index = held.index_of("pyright").expect("pyright is up");
    let slot = std::sync::Arc::clone(&held.slots()[index]);
    drop(held.stop(index));
    let admission = host.lifetime.admit().unwrap();

    let (activated, activation) = mpsc::channel();
    let demand = {
        let session = session.clone();
        let scope = fixture.root.join("src/helper.py");
        std::thread::spawn(move || {
            session.activate(Some(&scope));
            activated.send(()).unwrap();
        })
    };
    let mut pid = None;
    assert!(
        fixtures::wait_until(|| {
            pid = fixture
                .events()
                .into_iter()
                .find(|event| event.kind == "SYMBOLS" && !before.contains(&event.pid))
                .map(|event| event.pid);
            pid.is_some()
        }),
        "the new launch warmed its index: {:?}",
        fixture.events()
    );
    let pid = pid.unwrap();
    assert_eq!(
        host.lifetime.song.guard_count(),
        2,
        "the test's admission and the parked launcher's are both outstanding"
    );

    host.request_shutdown();
    let (done, completed) = mpsc::channel();
    let waiter = {
        let host = std::sync::Arc::clone(&host);
        std::thread::spawn(move || {
            host.wait_shutdown();
            done.send(()).unwrap();
        })
    };
    assert!(
        activation.try_recv().is_err(),
        "the launch cannot leave Starting while the lock is held"
    );
    assert!(completed.try_recv().is_err());

    drop(held);
    activation
        .recv_timeout(DEADLOCK)
        .expect("the parked launch ends once the lock is free");
    demand.join().unwrap();
    let state = slot.state();
    assert_eq!(state.label(), "failed", "{state:?}");
    assert!(
        state
            .reason()
            .unwrap_or_default()
            .contains("shut down while this server was starting"),
        "the launcher saw the claim under the lock: {state:?}"
    );
    assert!(
        !alive(pid),
        "the launcher reaped its own child before leaving Starting"
    );
    assert!(
        session.workspace().index_of("pyright").is_none(),
        "nothing was published"
    );
    assert!(
        completed.try_recv().is_err(),
        "the sweep is still waiting on the test's admission"
    );

    drop(admission);
    completed
        .recv_timeout(DEADLOCK)
        .expect("the shutdown completes once every admission is back");
    waiter.join().unwrap();
    assert!(
        session
            .workspace()
            .slots()
            .iter()
            .all(|slot| slot.state().label() == "not started"),
        "the sweep's cold is the last word on every slot"
    );
    assert!(
        before.iter().copied().all(|pid| !alive(pid)),
        "the sweep reaped what was running: {before:?}"
    );
}

/// Two `shutdown` calls at once share one shutdown, and neither returns
/// until its sweep has reaped every child (`docs/shutdown.md`). The MCP
/// front end never makes two; an embedded host with several threads can.
///
/// The first caller is held **inside** the sweep, not before it: the test
/// holds the workspace lock without admission, so the first caller passes
/// the completion wait, takes every entry out of the map, and then waits
/// for that lock. An empty map is the sign it got there. A second caller
/// arriving then finds nothing left to sweep; without the shared
/// completion it would return while the first caller's children still
/// run. Each caller checks the children itself the moment it returns.
#[test]
fn concurrent_shutdowns_each_return_after_the_one_sweep() {
    let fixture = fixture!("concurrent-shutdown");
    let host = std::sync::Arc::new(Host::new());
    let session = bound(&host, "a", &fixture.root);
    let pids = fixture.launches();
    assert!(!pids.is_empty(), "the workspace started its servers");

    let held = session.workspace();
    let (returned, returns) = mpsc::channel();
    let caller = |name: &'static str, entering: Option<mpsc::Sender<()>>| {
        let host = std::sync::Arc::clone(&host);
        let pids = pids.clone();
        let returned = returned.clone();
        std::thread::spawn(move || {
            if let Some(entering) = entering {
                entering.send(()).unwrap();
            }
            host.shutdown();
            let reaped = pids.iter().copied().all(|pid| !alive(pid));
            returned.send((name, reaped)).unwrap();
        })
    };

    let first = caller("first", None);
    assert!(
        fixtures::wait_until(|| host.lock().is_empty()),
        "the first caller reached the sweep"
    );
    let (entering, entered) = mpsc::channel();
    let second = caller("second", Some(entering));
    entered.recv_timeout(DEADLOCK).unwrap();
    assert!(
        returns.try_recv().is_err(),
        "neither caller returns while the sweep is held"
    );
    assert!(
        pids.iter().copied().all(alive),
        "a soft shutdown leaves running servers to the sweep: {pids:?}"
    );

    drop(held);
    for _ in 0..2 {
        let (name, reaped) = returns
            .recv_timeout(DEADLOCK)
            .expect("both callers return once the lock is free");
        assert!(
            reaped,
            "the {name} caller returned before every child was reaped"
        );
    }
    first.join().unwrap();
    second.join().unwrap();

    // One state, and it only moves forward: a later call finds the
    // shutdown complete and returns, and nothing is admitted again.
    let (done, completed) = mpsc::channel();
    let third = {
        let host = std::sync::Arc::clone(&host);
        std::thread::spawn(move || {
            host.shutdown();
            done.send(()).unwrap();
        })
    };
    completed
        .recv_timeout(DEADLOCK)
        .expect("a shutdown after the shutdown returns");
    third.join().unwrap();
    assert!(host.is_shutting_down());
    assert!(session.acquire(None, None).is_err());
    assert!(
        session
            .workspace()
            .slots()
            .iter()
            .all(|slot| slot.state().label() == "not started"),
        "every slot is cold"
    );
}
