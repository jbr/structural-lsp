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
    let operation = session.acquire(None).unwrap();
    host.request_shutdown();
    assert!(session.acquire(None).is_err());
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
    let operation = session.acquire(None).unwrap();
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
