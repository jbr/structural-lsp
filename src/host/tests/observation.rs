//! What observation does, asserted against synthetic sources.
//!
//! The real watcher has timing of its own; the synthetic source has none,
//! so every claim here is about wiring — which root was watched, what a
//! batch adds, when a new language warms, what a release does to the
//! watcher — and never about a sleep. The one real-backend smoke is
//! `#[ignore]`, with the other backends the default suite does not trust.
//! No test here makes a semantic claim: a batch only ever means "re-read
//! the tree" (`docs/observation.md`), and the refresh it drives is
//! [`Workspace::refresh`](crate::workspace::Workspace::refresh).

use super::bound;
use super::fixtures::{ladder_fixture, selective_fixture, servers_launched};
use crate::clock::TestClock;
use crate::host::observation::{
    Batch, Driver, Event, Notify, Pending, Source, Stop, Synthetic, reduce,
};
use crate::host::{AgentId, Host};
use crate::workspace::SlotState;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// A host whose workspaces are observed through fresh synthetic sources,
/// and every `Driver` the factory handed out, in creation order.
///
/// The factory must work for any number of workspaces, so it cannot hand
/// one driver to one test: it parks each in the vector, and a test with
/// one workspace reads the first.
pub(super) fn synthetic_host() -> (Host, Arc<Mutex<Vec<Driver>>>) {
    let drivers: Arc<Mutex<Vec<Driver>>> = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&drivers);
    let host = Host::with_observation(
        Box::new(move |evidence, watched| {
            let (source, driver) = Synthetic::new(evidence, watched);
            captured.lock().unwrap().push(driver.clone());
            (
                Box::new(source) as Box<dyn Source>,
                Arc::new(driver) as Arc<dyn Stop>,
            )
        }),
        Arc::new(crate::clock::SystemClock),
    );
    (host, drivers)
}

/// The same, with the shared test clock, so a test advances time instead of
/// sleeping through the eviction interval.
pub(super) fn residency_host() -> (Host, Arc<Mutex<Vec<Driver>>>, Arc<TestClock>) {
    let drivers: Arc<Mutex<Vec<Driver>>> = Arc::new(Mutex::new(Vec::new()));
    let captured = Arc::clone(&drivers);
    let clock = TestClock::shared();
    let host = Host::with_observation(
        Box::new(move |evidence, watched| {
            let (source, driver) = Synthetic::new(evidence, watched);
            captured.lock().unwrap().push(driver.clone());
            (
                Box::new(source) as Box<dyn Source>,
                Arc::new(driver) as Arc<dyn Stop>,
            )
        }),
        Arc::clone(&clock) as Arc<dyn crate::clock::Clock>,
    );
    (host, drivers, clock)
}

/// The first workspace's driver. Every test here binds one workspace.
pub(super) fn first_driver(drivers: &Arc<Mutex<Vec<Driver>>>) -> Driver {
    drivers.lock().unwrap()[0].clone()
}

/// Bounded poll for a condition another thread produces. The bound is so
/// a failure reports rather than hangs; the condition is a fact about
/// state another thread has already written, never something a wait could
/// make true.
pub(super) fn wait_until(condition: impl Fn() -> bool) -> bool {
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
    let created = reduce(
        path,
        notify::EventKind::Create(notify::event::CreateKind::File),
    );
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
    let removed = reduce(
        path,
        notify::EventKind::Remove(notify::event::RemoveKind::File),
    );
    assert_eq!(
        removed,
        Some(Event::Removed {
            path: path.to_path_buf()
        })
    );
    // A read, the unset kind, and the uninterpretable kind: none says the
    // tree changed.
    assert_eq!(
        reduce(
            path,
            notify::EventKind::Access(notify::event::AccessKind::Read)
        ),
        None
    );
    assert_eq!(reduce(path, notify::EventKind::Any), None);
    assert_eq!(reduce(path, notify::EventKind::Other), None);
}

#[test]
fn the_synthetic_source_hands_out_what_is_pushed_and_ends_on_close() {
    let evidence = Arc::new(Pending::new());
    let watched = Arc::new(Pending::new());
    let (mut source, driver) = Synthetic::new(evidence, watched);
    assert!(driver.watched().is_empty(), "nothing was watched yet");
    driver.push(Batch {
        events: vec![Event::Overflow],
    });
    // The debounce is the synthetic source's to ignore: what coalesces is
    // what the test pushed.
    let batch = source
        .next_batch(Some(Duration::from_secs(1)))
        .expect("the batch pushed before the read");
    assert_eq!(batch.events, vec![Event::Overflow]);
    driver.close();
    assert!(source.next_batch(Some(Duration::from_secs(1))).is_none());
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
fn a_new_language_warms_in_an_interested_workspace() {
    let Some(rust_only) = selective_fixture(
        "obs-activate",
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
    // The tree gains Python. The batch says only that something happened;
    // the refresh lays out the slot, and the workspace has an attached
    // agent, so the weak signal warms the new language's server too.
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
    assert!(
        wait_until(|| servers_launched(&rust_only).contains(&"pyright".to_string())),
        "a new language in an interested workspace warms its server, not just its slot"
    );
    assert_eq!(
        servers_launched(&rust_only),
        ["pyright", "rust-analyzer"],
        "the batch warmed exactly the new language"
    );
    host.shutdown();
}

#[test]
fn a_new_language_does_not_warm_in_an_observed_only_workspace() {
    let Some(rust_only) = selective_fixture(
        "obs-uninterested",
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
    std::fs::write(rust_only.root.join("src/helper.py"), "def a():\n    pass\n").unwrap();
    // Interest ends, but the session holds the workspace, so it stays
    // observed: the watcher still reaches it, and the refresh still runs.
    host.agent_ended(&AgentId::new("a"));
    // Two batches, because the thread hands out serially: the second
    // handout proves the first batch's refresh — and the warm decision it
    // made — has finished, so the absence asserted next is about completed
    // work rather than about timing.
    driver.push(Batch {
        events: vec![Event::Created {
            path: rust_only.root.join("src/helper.py"),
        }],
    });
    assert!(
        wait_until(|| driver.consumed() >= 1),
        "the first batch was handed out"
    );
    driver.push(Batch {
        events: vec![Event::Overflow],
    });
    assert!(
        wait_until(|| driver.consumed() >= 2),
        "both batches were handed out"
    );
    assert!(
        names_server(&session, "pyright"),
        "an observed-only workspace still updates evidence: the slot is laid out"
    );
    assert_eq!(
        servers_launched(&rust_only),
        ["rust-analyzer"],
        "an observed-only workspace updates evidence and launches nothing"
    );
    host.shutdown();
}

#[test]
fn a_query_demand_starts_a_new_language() {
    let Some(rust_only) = selective_fixture(
        "obs-demand",
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
    std::fs::write(rust_only.root.join("src/helper.py"), "def a():\n    pass\n").unwrap();
    // Uninterested: the new language gets a slot, and no launch.
    host.agent_ended(&AgentId::new("a"));
    driver.push(Batch {
        events: vec![Event::Created {
            path: rust_only.root.join("src/helper.py"),
        }],
    });
    assert!(
        wait_until(|| names_server(&session, "pyright")),
        "the refresh laid out the Python slot"
    );
    assert_eq!(
        servers_launched(&rust_only),
        ["rust-analyzer"],
        "nothing launched on the weak signal alone"
    );
    // A query is a strong signal: it starts the required server and waits
    // for readiness, whether or not an agent is attached — and it routes
    // through the entry's slot copy, which the refresh grew in step.
    let helper = rust_only.root.join("src/helper.py");
    session.activate(Some(helper.as_path()));
    assert_eq!(
        servers_launched(&rust_only),
        ["pyright", "rust-analyzer"],
        "the query started the new language's server"
    );
    let indices = session
        .workspace()
        .select(Some(helper.as_path()))
        .expect("the demand-started server answers for its scope");
    assert!(!indices.is_empty());
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
    // completed work, not about timing. The first is waited for
    // before the second is pushed: one wake that sees both would
    // hand them out as one.
    let ghost = rust_only.root.join("src/ghost.py");
    driver.push(Batch {
        events: vec![Event::Created {
            path: ghost.clone(),
        }],
    });
    assert!(
        wait_until(|| driver.consumed() >= 1),
        "the first ghost batch was handed out"
    );
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
    // Whether that slot warms is the signal policy, covered by the
    // interested / observed-only / demand tests above — this test is only
    // about what a batch's content is trusted for.
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
    // and re-merges, and no slot is doubled. The first is waited for
    // before the second is pushed: one wake that sees both would
    // hand them out as one.
    driver.push(Batch {
        events: vec![Event::Overflow],
    });
    assert!(
        wait_until(|| driver.consumed() >= 1),
        "the first batch was handed out"
    );
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
fn a_released_workspace_stops_observing_at_once() {
    let Some(rust_only) = selective_fixture(
        "obs-weak",
        &[("Cargo.toml", ""), ("src/lib.rs", "pub fn a() {}\n")],
        "",
    ) else {
        return;
    };
    let (host, drivers) = synthetic_host();
    let session = bound(&host, "a", &rust_only.root);
    let driver = first_driver(&drivers);
    // The watcher is alive while the workspace is held: it hands out a batch.
    driver.push(Batch {
        events: vec![Event::Overflow],
    });
    assert!(
        wait_until(|| driver.consumed() >= 1),
        "the watcher was alive and handed out the batch"
    );
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
    // A released-but-held workspace keeps observing: the batch is drained,
    // and the sweep keeps running under the holding session.
    driver.push(Batch {
        events: vec![Event::Overflow],
    });
    assert!(
        wait_until(|| driver.consumed() >= 2),
        "release did not end observation while a session still holds the workspace"
    );
    drop(session);
    // The last session has dropped: the watcher is closed at once, and the
    // thread ends on it — not at the tree's next event.
    assert!(
        wait_until(|| driver.finished()),
        "the entry's death closed the source and the observation thread ended"
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
    let (mut source, _stop) = Notify::new(Arc::new(Pending::new()), Arc::new(Pending::new()));
    let dir = std::env::temp_dir().join(format!("structural-notify-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    source
        .watch(&dir)
        .expect("the watcher watched the scratch root");
    std::fs::write(dir.join("a.txt"), "x\n").unwrap();
    let (sender, receiver) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = sender.send(source.next_batch(Some(Duration::from_millis(100))));
    });
    let batch = receiver
        .recv_timeout(Duration::from_secs(10))
        .expect("the backend yielded within the bound")
        .expect("a batch, not a closed source");
    assert!(!batch.events.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn an_idle_server_is_stopped_and_the_next_query_reacquires_it() {
    let Some(both) = selective_fixture(
        "res-evict",
        &[
            ("Cargo.toml", ""),
            ("src/lib.rs", "pub fn a() {}\n"),
            ("src/helper.py", "def a():\n    pass\n"),
        ],
        "",
    ) else {
        return;
    };
    let (host, drivers, clock) = residency_host();
    let session = bound(&host, "a", &both.root);
    let driver = first_driver(&drivers);
    let state_of = |name: &str| {
        session
            .workspace()
            .slots()
            .iter()
            .find(|slot| slot.name() == name)
            .expect("the fixture plans it")
            .state()
    };
    // Binding warms everything, and both servers published.
    assert_eq!(state_of("pyright"), SlotState::Running);
    assert_eq!(state_of("rust-analyzer"), SlotState::Running);
    // Thirty-one minutes pass with no strong signal addressed to either.
    // Two wakes: the sweep runs inside the first, and the second's handout
    // happens only after the first wake's iteration is done — the loop is
    // serial, so this is a claim about completed work, not timing.
    clock.advance(Duration::from_secs(31 * 60));
    driver.push(Batch { events: Vec::new() });
    driver.push(Batch { events: Vec::new() });
    assert!(wait_until(|| driver.consumed() >= 2), "both wakes drained");
    assert_eq!(
        state_of("pyright"),
        SlotState::Cold,
        "the idle sweep stopped a server no signal had addressed"
    );
    assert_eq!(
        state_of("rust-analyzer"),
        SlotState::Cold,
        "every idle server goes, not only the last queried"
    );
    // The next query reacquires exactly what it is about to use.
    let lib = both.source();
    session.activate(Some(lib.as_path()));
    assert_eq!(
        state_of("rust-analyzer"),
        SlotState::Running,
        "the query demand-started the evicted server"
    );
    assert_eq!(
        state_of("pyright"),
        SlotState::Cold,
        "the query for rust did not touch pyright"
    );
    let launched = servers_launched(&both);
    assert_eq!(
        launched.iter().filter(|s| **s == "rust-analyzer").count(),
        2,
        "the evicted server was launched a second time"
    );
    assert_eq!(
        launched.iter().filter(|s| **s == "pyright").count(),
        1,
        "pyright stayed stopped"
    );
    host.shutdown();
}

#[test]
fn idle_is_per_server() {
    let Some(both) = selective_fixture(
        "res-per-slot",
        &[
            ("Cargo.toml", ""),
            ("src/lib.rs", "pub fn a() {}\n"),
            ("src/helper.py", "def a():\n    pass\n"),
        ],
        "",
    ) else {
        return;
    };
    let (host, drivers, clock) = residency_host();
    let session = bound(&host, "a", &both.root);
    let driver = first_driver(&drivers);
    let state_of = |name: &str| {
        session
            .workspace()
            .slots()
            .iter()
            .find(|slot| slot.name() == name)
            .expect("the fixture plans it")
            .state()
    };
    // The bind addressed both servers; a query addresses only the ones its
    // scope routes to. Twenty minutes pass, then the query refreshes
    // Python's clock alone; ten more, and Rust's has run out from the bind
    // while Python's is still young.
    clock.advance(Duration::from_secs(20 * 60));
    let helper = both.root.join("src/helper.py");
    session.activate(Some(helper.as_path()));
    clock.advance(Duration::from_secs(10 * 60));
    driver.push(Batch { events: Vec::new() });
    driver.push(Batch { events: Vec::new() });
    assert!(wait_until(|| driver.consumed() >= 2), "both wakes drained");
    assert_eq!(
        state_of("pyright"),
        SlotState::Running,
        "the query refreshed the clock of the server it addressed"
    );
    assert_eq!(
        state_of("rust-analyzer"),
        SlotState::Cold,
        "the server no signal addressed since the bind expired"
    );
    host.shutdown();
}

#[test]
fn filesystem_batches_do_not_keep_a_server_alive() {
    let Some(both) = selective_fixture(
        "res-keepalive",
        &[
            ("Cargo.toml", ""),
            ("src/lib.rs", "pub fn a() {}\n"),
            ("src/helper.py", "def a():\n    pass\n"),
        ],
        "",
    ) else {
        return;
    };
    let (host, drivers, clock) = residency_host();
    let session = bound(&host, "a", &both.root);
    let driver = first_driver(&drivers);
    let state_of = |name: &str| {
        session
            .workspace()
            .slots()
            .iter()
            .find(|slot| slot.name() == name)
            .expect("the fixture plans it")
            .state()
    };
    // A real batch: evidence maintenance runs, and touches nothing —
    // filesystem activity is not a keepalive.
    driver.push(Batch {
        events: vec![Event::Created {
            path: both.root.join("src/other.rs"),
        }],
    });
    assert!(
        wait_until(|| driver.consumed() >= 1),
        "the batch was drained"
    );
    clock.advance(Duration::from_secs(31 * 60));
    driver.push(Batch {
        events: vec![Event::Created {
            path: both.root.join("src/other.rs"),
        }],
    });
    driver.push(Batch { events: Vec::new() });
    assert!(wait_until(|| driver.consumed() >= 3), "wakes drained");
    assert_eq!(
        state_of("pyright"),
        SlotState::Cold,
        "a batch that arrived inside the idle window did not extend it"
    );
    assert_eq!(state_of("rust-analyzer"), SlotState::Cold);
    host.shutdown();
}

#[test]
fn a_failed_slot_is_cleared_on_the_same_clock() {
    let Some(ladder) = ladder_fixture("res-failed", "missing", "missing") else {
        return;
    };
    let (host, drivers, clock) = residency_host();
    let session = bound(&host, "a", &ladder.root);
    let driver = first_driver(&drivers);
    let slot = || {
        session
            .workspace()
            .slots()
            .first()
            .expect("the rust plan's slot")
            .clone()
    };
    // The bind's warm walked the ladder; both candidates are missing.
    assert!(
        matches!(slot().state(), SlotState::Failed(_)),
        "the walk ended in a failure"
    );
    // The failure expires like a server does: the sweep clears it, so a
    // later demand walks the ladder again rather than being refused
    // forever — and not before, so a failure is never retried in a loop.
    clock.advance(Duration::from_secs(31 * 60));
    driver.push(Batch { events: Vec::new() });
    driver.push(Batch { events: Vec::new() });
    assert!(wait_until(|| driver.consumed() >= 2), "both wakes drained");
    assert_eq!(
        slot().state(),
        SlotState::Cold,
        "the failure was cleared on the clock"
    );
    assert!(
        slot().failed_attempts().is_empty(),
        "the cleared attempts described a launch that is over"
    );
    // The next demand walks the ladder from the top, and fails again —
    // freshly, with its own attempts recorded.
    session.activate(None);
    assert!(
        matches!(slot().state(), SlotState::Failed(_)),
        "the demand re-walked the ladder"
    );
    assert!(
        !slot().failed_attempts().is_empty(),
        "the second walk recorded its own attempts"
    );
    host.shutdown();
}

#[test]
fn eviction_leaves_observation_intact() {
    let Some(rust_only) = selective_fixture(
        "res-intact",
        &[("Cargo.toml", ""), ("src/lib.rs", "pub fn a() {}\n")],
        "",
    ) else {
        return;
    };
    let (host, drivers, clock) = residency_host();
    let session = bound(&host, "a", &rust_only.root);
    let driver = first_driver(&drivers);
    clock.advance(Duration::from_secs(31 * 60));
    driver.push(Batch { events: Vec::new() });
    driver.push(Batch { events: Vec::new() });
    assert!(wait_until(|| driver.consumed() >= 2), "both wakes drained");
    let stopped = session
        .workspace()
        .slots()
        .iter()
        .all(|slot| slot.state() == SlotState::Cold);
    assert!(stopped, "the sweep stopped what the bind had warmed");
    // The tree gains Python. The batch still refreshes evidence and lays
    // out the new language's slot, and the workspace still has an attached
    // agent, so the weak signal still warms it — eviction took the
    // servers, not the observation.
    std::fs::write(rust_only.root.join("src/helper.py"), "def a():\n    pass\n").unwrap();
    driver.push(Batch {
        events: vec![Event::Created {
            path: rust_only.root.join("src/helper.py"),
        }],
    });
    driver.push(Batch { events: Vec::new() });
    assert!(wait_until(|| driver.consumed() >= 4), "wakes drained");
    assert!(
        wait_until(|| names_server(&session, "pyright")),
        "observation still grows the slot list after eviction"
    );
    assert!(
        wait_until(|| servers_launched(&rust_only).contains(&"pyright".to_string())),
        "observation still warms a new language after eviction"
    );
    assert_eq!(
        servers_launched(&rust_only)
            .iter()
            .filter(|s| **s == "rust-analyzer")
            .count(),
        1,
        "the batch did not relaunch what the sweep had stopped"
    );
    host.shutdown();
}

#[test]
fn a_watch_failure_degrades_to_a_ticker_and_recovers() {
    let Some(rust_only) = selective_fixture(
        "res-watch-lost",
        &[("Cargo.toml", ""), ("src/lib.rs", "pub fn a() {}\n")],
        "",
    ) else {
        return;
    };
    let (host, drivers, clock) = residency_host();
    let session = bound(&host, "a", &rust_only.root);
    let driver = first_driver(&drivers);
    let state_of = || {
        session
            .workspace()
            .slots()
            .first()
            .expect("the rust plan's slot")
            .state()
    };
    // The watch is lost, and every re-establishment fails: no refreshes
    // will happen, but the sweep keeps running, because it needs only a
    // clock.
    driver.lose();
    driver.fail_watch(true);
    driver.push(Batch { events: Vec::new() });
    driver.push(Batch { events: Vec::new() });
    assert!(wait_until(|| driver.consumed() >= 2), "wakes drained");
    clock.advance(Duration::from_secs(31 * 60));
    driver.push(Batch { events: Vec::new() });
    driver.push(Batch { events: Vec::new() });
    assert!(wait_until(|| driver.consumed() >= 4), "wakes drained");
    assert_eq!(
        state_of(),
        SlotState::Cold,
        "the idle sweep ran with the watch lost"
    );
    // The watch recovers: the sweep's retry succeeds, and a fresh watch is
    // on the tree.
    driver.fail_watch(false);
    driver.push(Batch { events: Vec::new() });
    driver.push(Batch { events: Vec::new() });
    assert!(wait_until(|| driver.consumed() >= 6), "wakes drained");
    assert_eq!(
        driver.watched().len(),
        2,
        "the recovered watch watched the root a second time"
    );
    // Evidence maintenance resumes: the tree gains Python, the batch
    // refreshes, and the attached agent's weak signal warms it.
    std::fs::write(rust_only.root.join("src/helper.py"), "def a():\n    pass\n").unwrap();
    driver.push(Batch {
        events: vec![Event::Created {
            path: rust_only.root.join("src/helper.py"),
        }],
    });
    driver.push(Batch { events: Vec::new() });
    assert!(wait_until(|| driver.consumed() >= 8), "wakes drained");
    assert!(
        wait_until(|| names_server(&session, "pyright")),
        "a recovered watch refreshes evidence again"
    );
    assert!(
        wait_until(|| servers_launched(&rust_only).contains(&"pyright".to_string())),
        "a recovered watch warms a new language again"
    );
    host.shutdown();
}
