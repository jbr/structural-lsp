//! What observation does, asserted against synthetic sources.
//!
//! The real watcher has timing of its own; the synthetic source has none,
//! so every claim here is about wiring and reduction — which root was
//! watched, what a batch adds, what a release does to the watcher — and
//! never about a sleep. The one real-backend smoke is `#[ignore]`, with
//! the other backends the default suite does not trust. No test here
//! makes a semantic claim: a batch only ever means "re-read the tree"
//! (`backlog/discovery/observation.md`), and the refresh it drives is
//! [`Workspace::refresh`](crate::workspace::Workspace::refresh).

use super::bound;
use super::fixtures::{selective_fixture, servers_launched};
use crate::host::observation::{
    Batch, Driver, Event, Notify, Source, Synthetic, reduce,
};
use crate::host::{AgentId, Host};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// A host whose workspaces are observed through fresh synthetic sources,
/// and every `Driver` the factory handed out, in creation order.
///
/// The factory must work for any number of workspaces, so it cannot hand
/// one driver to one test: it parks each in the vector, and a test with
/// one workspace reads the first.
fn synthetic_host() -> (Host, Arc<Mutex<Vec<Driver>>>) {
    let drivers: Arc<Mutex<Vec<Driver>>> = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&drivers);
    let host = Host::with_observation(Box::new(move || {
        let (source, driver) = Synthetic::new();
        captured.lock().unwrap().push(driver);
        Box::new(source) as Box<dyn Source>
    }));
    (host, drivers)
}

/// The first workspace's driver. Every test here binds one workspace.
fn first_driver(drivers: &Arc<Mutex<Vec<Driver>>>) -> Driver {
    drivers.lock().unwrap()[0].clone()
}

/// Bounded poll for a condition another thread produces. The bound is so
/// a failure reports rather than hangs; the condition is a fact about
/// state another thread has already written, never something a wait could
/// make true.
fn wait_until(condition: impl Fn() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if condition() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    condition()
}

/// Whether any slot's ladder names `server`.
fn names_server(session: &crate::session::Session, server: &str) -> bool {
    session
        .workspace()
        .slots()
        .iter()
        .any(|slot| slot.candidate_names().contains(&server))
}

#[test]
fn the_reduction_keeps_the_three_changes_and_drops_the_rest() {
    let path = std::path::Path::new("/tmp/whatever/helper.py");
    let created = reduce(path, notify::EventKind::Create(notify::event::CreateKind::File));
    assert_eq!(
        created,
        Some(Event::Created {
            path: path.to_path_buf()
        })
    );
    let changed = reduce(
        path,
        notify::EventKind::Modify(notify::event::ModifyKind::Data(
            notify::event::DataChange::Any,
        )),
    );
    assert_eq!(
        changed,
        Some(Event::Changed {
            path: path.to_path_buf()
        })
    );
    let removed = reduce(path, notify::EventKind::Remove(notify::event::RemoveKind::File));
    assert_eq!(
        removed,
        Some(Event::Removed {
            path: path.to_path_buf()
        })
    );
    // A read, the unset kind, and the uninterpretable kind: none says the
    // tree changed.
    assert_eq!(
        reduce(path, notify::EventKind::Access(notify::event::AccessKind::Read)),
        None
    );
    assert_eq!(reduce(path, notify::EventKind::Any), None);
    assert_eq!(reduce(path, notify::EventKind::Other), None);
}

#[test]
fn the_synthetic_source_hands_out_what_is_pushed_and_ends_on_close() {
    let (mut source, driver) = Synthetic::new();
    assert!(driver.watched().is_empty(), "nothing was watched yet");
    driver.push(Batch {
        events: vec![Event::Overflow],
    });
    // The debounce is the synthetic source's to ignore: what coalesces is
    // what the test pushed.
    let batch = source
        .next_batch(Duration::from_secs(1))
        .expect("the batch pushed before the read");
    assert_eq!(batch.events, vec![Event::Overflow]);
    driver.close();
    assert!(source.next_batch(Duration::from_secs(1)).is_none());
    assert_eq!(driver.consumed(), 1, "one batch was handed out");
}

#[test]
fn the_observer_watches_the_workspace_root() {
    let Some(fixture) = selective_fixture(
        "obs-root",
        &[("Cargo.toml", ""), ("src/lib.rs", "pub fn a() {}\n")],
        "",
    ) else {
        return;
    };
    let (host, drivers) = synthetic_host();
    let session = bound(&host, "a", &fixture.root);
    // The canonical root the workspace holds is what the observer is
    // pointed at — not a caller's cwd, not a subdirectory.
    let root = session.workspace().root().to_path_buf();
    let driver = first_driver(&drivers);
    assert!(
        wait_until(|| driver.watched() == vec![root.clone()]),
        "the observer watched {:?}, not the workspace root {:?}",
        driver.watched(),
        root
    );
    host.shutdown();
}

#[test]
fn a_batch_lays_out_a_slot_for_a_language_the_tree_gained() {
    let Some(rust_only) = selective_fixture(
        "obs-grow",
        &[("Cargo.toml", ""), ("src/lib.rs", "pub fn a() {}\n")],
        "",
    ) else {
        return;
    };
    let (host, drivers) = synthetic_host();
    let session = bound(&host, "a", &rust_only.root);
    let driver = first_driver(&drivers);
    assert!(
        wait_until(|| !session.workspace().slots().is_empty()),
        "the initial layout landed"
    );
    assert!(
        !names_server(&session, "pyright"),
        "the tree had no Python, so the initial layout has no Python slot"
    );
    // The tree gains Python; the batch says only that something happened.
    std::fs::write(rust_only.root.join("src/helper.py"), "def a():\n    pass\n").unwrap();
    driver.push(Batch {
        events: vec![Event::Created {
            path: rust_only.root.join("src/helper.py"),
        }],
    });
    assert!(
        wait_until(|| names_server(&session, "pyright")),
        "the refreshed layout grew a Python slot"
    );
    // A refresh lays out slots and starts none of them: the Python mock
    // never launched, though the slot names it.
    assert_eq!(
        servers_launched(&rust_only),
        ["rust-analyzer"],
        "a refresh never warms"
    );
    host.shutdown();
}

#[test]
fn batch_content_is_not_trusted() {
    let Some(rust_only) = selective_fixture(
        "obs-notrust",
        &[("Cargo.toml", ""), ("src/lib.rs", "pub fn a() {}\n")],
        "",
    ) else {
        return;
    };
    let (host, drivers) = synthetic_host();
    let session = bound(&host, "a", &rust_only.root);
    let driver = first_driver(&drivers);
    assert!(
        wait_until(|| !session.workspace().slots().is_empty()),
        "the initial layout landed"
    );
    // Two batches naming a file that does not exist. Two, because the
    // thread hands out serially: the second handout proves the first
    // batch's refresh has finished, so what is asserted next is about
    // completed work, not about timing.
    let ghost = rust_only.root.join("src/ghost.py");
    driver.push(Batch {
        events: vec![Event::Created { path: ghost.clone() }],
    });
    driver.push(Batch {
        events: vec![Event::Created { path: ghost }],
    });
    assert!(
        wait_until(|| driver.consumed() >= 2),
        "both ghost batches were handed out"
    );
    assert!(
        !names_server(&session, "pyright"),
        "a batch's content never adds a slot by itself"
    );
    // Now the tree really gains Python, and the batch says only
    // "something happened" — an overflow, as if events had been lost.
    std::fs::write(rust_only.root.join("src/helper.py"), "def a():\n    pass\n").unwrap();
    driver.push(Batch {
        events: vec![Event::Overflow],
    });
    assert!(
        wait_until(|| names_server(&session, "pyright")),
        "the rescan reconciled what the batch content never carried"
    );
    assert_eq!(
        servers_launched(&rust_only),
        ["rust-analyzer"],
        "a refresh never warms"
    );
    host.shutdown();
}

#[test]
fn a_batch_that_changes_nothing_lays_out_nothing() {
    let Some(polyglot) = selective_fixture(
        "obs-idle",
        &[
            ("Cargo.toml", ""),
            ("src/lib.rs", "pub fn a() {}\n"),
            ("src/helper.py", "def a():\n    pass\n"),
        ],
        "",
    ) else {
        return;
    };
    let (host, drivers) = synthetic_host();
    let session = bound(&host, "a", &polyglot.root);
    let driver = first_driver(&drivers);
    assert!(
        wait_until(|| session.workspace().slots().len() == 2),
        "the initial layout has both languages"
    );
    // Two batches over an unchanged tree: refresh re-gathers, re-selects,
    // and re-merges, and no slot is doubled.
    driver.push(Batch {
        events: vec![Event::Overflow],
    });
    driver.push(Batch {
        events: vec![Event::Overflow],
    });
    assert!(
        wait_until(|| driver.consumed() >= 2),
        "both batches were handed out"
    );
    assert_eq!(
        session.workspace().slots().len(),
        2,
        "a refresh never duplicates a language's slot"
    );
    assert_eq!(
        servers_launched(&polyglot),
        ["pyright", "rust-analyzer"],
        "and nothing new launched"
    );
    host.shutdown();
}

#[test]
fn a_released_workspace_is_not_kept_alive_by_its_watcher() {
    let Some(rust_only) = selective_fixture(
        "obs-weak",
        &[("Cargo.toml", ""), ("src/lib.rs", "pub fn a() {}\n")],
        "",
    ) else {
        return;
    };
    let (host, drivers) = synthetic_host();
    let session = bound(&host, "a", &rust_only.root);
    // The observer thread is parked on the synthetic source, holding — by
    // design — nothing but the Weak.
    let weak = {
        let workspaces = host.lock();
        let entry = workspaces
            .values()
            .next()
            .expect("the bound workspace")
            .clone();
        Arc::downgrade(&entry)
    };
    host.agent_ended(&AgentId::new("a"));
    drop(session);
    let driver = first_driver(&drivers);
    driver.push(Batch {
        events: vec![Event::Overflow],
    });
    assert!(
        wait_until(|| driver.consumed() >= 1),
        "the watcher was alive and handed out the batch"
    );
    assert!(
        weak.upgrade().is_none(),
        "the released workspace is gone: the watcher holds nothing, not even while parked"
    );
    host.shutdown();
}

/// The one claim the synthetic source cannot carry: the real backend
/// yields a batch for a real write. Ignored like the other backend tests
/// — it costs real time and depends on the platform watcher.
#[test]
#[ignore = "a real backend: run with --ignored"]
fn a_real_watcher_yields_a_batch() {
    let mut source = Notify::new();
    let dir = std::env::temp_dir().join(format!("structural-notify-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    source.watch(&dir).expect("the watcher watched the scratch root");
    std::fs::write(dir.join("a.txt"), "x\n").unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(source.next_batch(Duration::from_millis(100)));
    });
    let batch = receiver
        .recv_timeout(Duration::from_secs(10))
        .expect("the backend yielded within the bound")
        .expect("a batch, not a closed source");
    assert!(!batch.events.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}