//! A child that never becomes a server is reaped by the value that spawned
//! it.
//!
//! A launch relies on this on every path that does not publish
//! (`docs/shutdown.md`): a registration refused because the host began
//! stopping drops the [`Spawning`] it holds, and a rung whose handshake
//! fails gets an `Err` from [`Spawning::initialize`] and moves on to the
//! next candidate. Neither path stops or reaps the child itself; the
//! [`ServerHandle`](crate::lsp::handle::ServerHandle) the `Spawning` owns
//! does, in its `Drop`, before that drop returns. These tests assert it at
//! the pid level, on the host mock, with the child alive at the moment of
//! failure wherever that can be arranged.

use super::*;
use crate::lsp::adapter::Language;
use crate::test_support::{HOST_MOCK_PY, python, reaped};
use std::time::{Duration, Instant};

/// The host mock behind a minimal adapter: no base server, so nothing
/// server-specific changes what the handshake says or waits for.
struct Mocked {
    command: Vec<String>,
}

impl Adapter for Mocked {
    fn name(&self) -> &str {
        "mock"
    }
    fn command(&self) -> Vec<String> {
        self.command.clone()
    }
    fn languages(&self) -> Vec<Language> {
        vec![Language::new("python", &["py"])]
    }
}

/// A scratch root and a mock launched in `mode` (`HOST_MOCK_PY`'s fourth
/// argument), with the script and its log outside the root.
struct Scratch {
    root: PathBuf,
    log: PathBuf,
    adapter: Arc<dyn Adapter>,
}

fn scratch(tag: &str, mode: Option<&str>) -> Option<Scratch> {
    let python = python()?;
    let base =
        std::env::temp_dir().join(format!("structural-spawning-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let root = base.join("root");
    std::fs::create_dir_all(&root).unwrap();
    let script = base.join("mock.py");
    std::fs::write(&script, HOST_MOCK_PY).unwrap();
    let log = base.join("launches.log");
    let mut command = vec![
        python,
        script.display().to_string(),
        log.display().to_string(),
        "mock".to_string(),
    ];
    command.extend(mode.map(str::to_string));
    Some(Scratch {
        root,
        log,
        adapter: Arc::new(Mocked { command }),
    })
}

impl Scratch {
    /// The pid the mock recorded on start. The child writes it before it
    /// reads anything, so this waits on a signal the child produces; the
    /// bound is there so a child that never starts fails the test rather
    /// than hanging it.
    fn pid(&self) -> i32 {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let started = std::fs::read_to_string(&self.log).ok().and_then(|text| {
                text.lines()
                    .find_map(|line| line.strip_prefix("START mock ")?.trim().parse().ok())
            });
            if let Some(pid) = started {
                return pid;
            }
            assert!(
                Instant::now() < deadline,
                "the mock never recorded its start"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

/// Whether `pid` is a live process — or a zombie, which signal 0 also
/// reaches.
fn exists(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

/// The registration-refused path: a `Spawning` dropped without a handshake
/// leaves no child, live or zombie, once the drop returns.
#[test]
fn a_spawned_server_dropped_before_its_handshake_is_reaped() {
    let Some(scratch) = scratch("dropped", None) else {
        eprintln!("skipping: python3 not available");
        return;
    };
    let spawning = LspServer::spawn(Arc::clone(&scratch.adapter), &scratch.root).unwrap();
    let pid = scratch.pid();
    assert!(exists(pid), "the child is running before the drop");
    drop(spawning);
    reaped(pid);
}

/// Every way `initialize` can fail after the child exists returns only once
/// the child is reaped: before anything is sent (a root with no URI), after
/// the server answered (an encoding this client does not convert), and
/// when the server died first (its exit is collected, not left a zombie).
#[test]
fn a_failed_handshake_reaps_its_child_before_returning() {
    let cases: [(&str, Option<&str>, &str); 3] = [
        ("relative-root", None, "is not absolute"),
        ("encoding", Some("utf-32"), "position encoding"),
        ("dies", Some("die"), "initializing mock"),
    ];
    for (tag, mode, expected) in cases {
        let Some(scratch) = scratch(tag, mode) else {
            eprintln!("skipping: python3 not available");
            return;
        };
        let root = if tag == "relative-root" {
            PathBuf::from(format!("structural-relative-root-{}", std::process::id()))
        } else {
            scratch.root.clone()
        };
        let spawning = LspServer::spawn(Arc::clone(&scratch.adapter), &root).unwrap();
        let pid = scratch.pid();
        let error = match spawning.initialize() {
            Ok(_) => panic!("{tag}: the handshake was arranged to fail"),
            Err(error) => format!("{error:#}"),
        };
        assert!(error.contains(expected), "{tag}: {error}");
        reaped(pid);
    }
}
