//! What reaches the operator, asserted against a recording sink: each
//! transition once, at the transition, and nothing for a launch that went
//! as planned (`docs/operator.md`).

use super::bound;
use super::fixtures::{asymmetric_fixture, ladder_fixture, selective_fixture};
use super::observation::{first_driver, residency_host, synthetic_host, wait_until};
use crate::host::Host;
use crate::host::observation::{Batch, Event as Change};
use crate::operator::{Kind, Level, Operator};
use crate::test_support::Recorder;
use std::sync::Arc;
use std::time::Duration;

fn recorded(host: Host) -> (Host, Arc<Recorder>) {
    let recorder = Recorder::shared();
    let host = host.with_operator(Arc::clone(&recorder) as Arc<dyn Operator>);
    (host, recorder)
}

/// A server that will not start is reported once — when its launch fails —
/// however many queries later find the slot failed and say so themselves.
#[test]
fn a_launch_failure_reaches_the_operator_exactly_once() {
    let Some(fixture) = asymmetric_fixture("op-once", "no-such-rust-server") else {
        return;
    };
    let (host, recorder) = recorded(Host::new());
    let session = bound(&host, "a", &fixture.root);
    // Two more demands for the failed server: each is told, the operator is
    // not told again.
    session.activate(Some(&fixture.source()));
    session.activate(Some(&fixture.source()));

    let failed = recorder.of(Kind::LaunchFailed);
    assert_eq!(failed.len(), 1, "{:?}", recorder.events());
    let event = &failed[0];
    assert_eq!(event.level, Level::Error);
    assert_eq!(event.server.as_deref(), Some("rust-analyzer"));
    assert_eq!(
        event.root,
        std::fs::canonicalize(&fixture.root).unwrap(),
        "attributed to the workspace"
    );
    assert!(
        event.message.starts_with("failed to start:")
            && event.message.contains("no-such-rust-server")
            && event.message.contains("to fix:"),
        "{}",
        event.message
    );
    assert_eq!(
        recorder.events().len(),
        1,
        "a one-rung ladder has no candidate event, and pyright started as planned: {:?}",
        recorder.events()
    );
    host.shutdown();
}

/// A fallback is two transitions: the candidate that failed, as it failed,
/// and the server serving in its place, once published.
#[test]
fn a_fallback_reports_the_failed_candidate_and_then_the_server_serving() {
    let Some(fixture) = ladder_fixture("op-fallback", "dies", "serves") else {
        return;
    };
    let (host, recorder) = recorded(Host::new());
    let _session = bound(&host, "a", &fixture.root);

    let events = recorder.events();
    let kinds: Vec<Kind> = events.iter().map(|event| event.kind).collect();
    assert_eq!(kinds, [Kind::CandidateFailed, Kind::Fallback], "{events:?}");
    assert_eq!(events[0].server.as_deref(), Some("a-preferred"));
    assert_eq!(events[0].level, Level::Warning);
    assert!(
        events[0].message.ends_with("trying b-alternative next"),
        "{}",
        events[0].message
    );
    assert_eq!(events[1].server.as_deref(), Some("b-alternative"));
    assert_eq!(events[1].level, Level::Warning);
    assert!(
        events[1].message.contains("in place of a-preferred"),
        "{}",
        events[1].message
    );
    host.shutdown();
}

/// A ladder that runs out ends in one error naming every candidate, after
/// the warning for each one that had a next.
#[test]
fn an_exhausted_ladder_ends_in_one_error_naming_every_candidate() {
    let Some(fixture) = ladder_fixture("op-exhausted", "missing", "missing") else {
        return;
    };
    let (host, recorder) = recorded(Host::new());
    let _session = bound(&host, "a", &fixture.root);

    let events = recorder.events();
    let kinds: Vec<Kind> = events.iter().map(|event| event.kind).collect();
    assert_eq!(
        kinds,
        [Kind::CandidateFailed, Kind::LaunchFailed],
        "{events:?}"
    );
    let failed = &events[1];
    assert_eq!(failed.level, Level::Error);
    assert_eq!(failed.server.as_deref(), Some("a-preferred"));
    assert!(
        failed.message.contains("none of 2 candidates started")
            && failed.message.contains("no-such-server-a-preferred")
            && failed.message.contains("no-such-server-b-alternative"),
        "{}",
        failed.message
    );
    host.shutdown();
}

/// The control: a ladder whose first candidate starts says nothing.
#[test]
fn a_launch_that_goes_as_planned_reports_nothing() {
    let Some(fixture) = ladder_fixture("op-control", "serves", "serves") else {
        return;
    };
    let (host, recorder) = recorded(Host::new());
    let _session = bound(&host, "a", &fixture.root);
    assert!(recorder.events().is_empty(), "{:?}", recorder.events());
    host.shutdown();
}

/// A language the tree holds that nothing serves is reported once per
/// workspace: at the first selection, not again at a refresh that still
/// finds it, and a language newly unserved is reported when it appears.
#[test]
fn an_unserved_language_is_reported_once_per_workspace() {
    // The catalogue serves neither Java nor Kotlin; `pom.xml` implies one
    // and `build.gradle` both.
    let Some(fixture) = selective_fixture(
        "op-unserved",
        &[
            ("Cargo.toml", ""),
            ("src/lib.rs", "pub fn a() {}\n"),
            ("pom.xml", "<project/>\n"),
        ],
        "",
    ) else {
        return;
    };
    let (host, drivers) = synthetic_host();
    let (host, recorder) = recorded(host);
    let _session = bound(&host, "a", &fixture.root);
    let driver = first_driver(&drivers);
    let unserved = || -> Vec<String> {
        recorder
            .of(Kind::Unserved)
            .iter()
            .map(|event| event.message.split(' ').next().unwrap_or("").to_string())
            .collect()
    };
    assert_eq!(unserved(), ["java"]);
    let java = &recorder.of(Kind::Unserved)[0];
    assert_eq!(java.level, Level::Warning);
    assert!(java.server.is_none());
    assert!(
        java.message.contains("`pom.xml` implies java"),
        "{}",
        java.message
    );

    // A refresh that still finds Java says nothing new.
    let lib = fixture.source();
    driver.push(Batch {
        events: vec![Change::Changed { path: lib.clone() }],
    });
    driver.push(Batch { events: Vec::new() });
    assert!(wait_until(|| driver.consumed() >= 2), "wakes drained");
    assert_eq!(unserved(), ["java"]);

    // The tree gains Kotlin: reported at the refresh that finds it, and
    // Java still once.
    std::fs::write(fixture.root.join("build.gradle"), "").unwrap();
    driver.push(Batch {
        events: vec![Change::Created {
            path: fixture.root.join("build.gradle"),
        }],
    });
    driver.push(Batch { events: Vec::new() });
    assert!(wait_until(|| driver.consumed() >= 4), "wakes drained");
    assert_eq!(unserved(), ["java", "kotlin"]);
    host.shutdown();
}

/// The idle sweep's release is reported, with the server it stopped.
#[test]
fn a_server_the_sweep_releases_is_reported() {
    let Some(fixture) = selective_fixture(
        "op-released",
        &[("Cargo.toml", ""), ("src/lib.rs", "pub fn a() {}\n")],
        "",
    ) else {
        return;
    };
    let (host, drivers, clock) = residency_host();
    let (host, recorder) = recorded(host);
    let _session = bound(&host, "a", &fixture.root);
    let driver = first_driver(&drivers);
    assert!(recorder.of(Kind::Released).is_empty());

    clock.advance(Duration::from_secs(31 * 60));
    driver.push(Batch { events: Vec::new() });
    driver.push(Batch { events: Vec::new() });
    assert!(wait_until(|| driver.consumed() >= 2), "wakes drained");

    let released = recorder.of(Kind::Released);
    assert_eq!(released.len(), 1, "{:?}", recorder.events());
    assert_eq!(released[0].level, Level::Info);
    assert_eq!(released[0].server.as_deref(), Some("rust-analyzer"));
    host.shutdown();
}

/// A lost watch is reported once however many retries fail, and its
/// recovery once.
#[test]
fn a_lost_watch_and_its_recovery_are_each_reported_once() {
    let Some(fixture) = selective_fixture(
        "op-watch",
        &[("Cargo.toml", ""), ("src/lib.rs", "pub fn a() {}\n")],
        "",
    ) else {
        return;
    };
    let (host, drivers) = synthetic_host();
    let (host, recorder) = recorded(host);
    let _session = bound(&host, "a", &fixture.root);
    let driver = first_driver(&drivers);

    driver.lose();
    driver.fail_watch(true);
    for _ in 0..3 {
        driver.push(Batch { events: Vec::new() });
    }
    assert!(wait_until(|| driver.consumed() >= 3), "wakes drained");
    assert_eq!(
        recorder.of(Kind::WatchLost).len(),
        1,
        "{:?}",
        recorder.events()
    );
    assert!(recorder.of(Kind::WatchRecovered).is_empty());

    driver.fail_watch(false);
    for _ in 0..3 {
        driver.push(Batch { events: Vec::new() });
    }
    assert!(wait_until(|| driver.consumed() >= 6), "wakes drained");
    let recovered = recorder.of(Kind::WatchRecovered);
    assert_eq!(recovered.len(), 1, "{:?}", recorder.events());
    assert_eq!(recovered[0].level, Level::Info);
    assert_eq!(recorder.of(Kind::WatchLost).len(), 1);
    host.shutdown();
}
