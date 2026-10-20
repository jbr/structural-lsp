//! The scratch workspaces these tests run against, and how a launch is
//! observed.
//!
//! One place, because every case here needs the same two things: a tree on
//! disk whose `structural.toml` points both built-in servers at the
//! scripted mock, and the mock's log, which records a line per launch. A
//! test asserting that a server did *not* start is asserting about that
//! log, so what it contains has to be a fact about the fixture rather than
//! about the test that reads it.

use crate::test_support::HOST_MOCK_PY;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// A scratch workspace root, and the mock script **outside** it.
///
/// The script is a `.py` file, and a workspace root is now read as
/// evidence of what the tree is (`docs/discovery.md`), so a mock living
/// inside the root would silently make every fixture a Python project.
/// That is not a hypothetical: it is what this helper was extracted to
/// fix.
pub(super) fn scratch(name: &str) -> (PathBuf, PathBuf) {
    let base = std::env::temp_dir().join(format!("structural-host-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    let dir = base.join("root");
    std::fs::create_dir_all(&dir).unwrap();
    let script = base.join("mock.py");
    std::fs::write(&script, HOST_MOCK_PY).unwrap();
    (dir, script)
}

/// A workspace on disk whose built-in servers are overridden to run the mock.
pub(super) struct Fixture {
    pub root: PathBuf,
    pub log: PathBuf,
    /// The mock script, so a test can write a `structural.toml` pointing
    /// at it somewhere other than the fixture root — which it must
    /// whenever the project root is not the fixture root, since
    /// configuration is read from the project root
    /// (`docs/discovery.md`).
    pub script: PathBuf,
    pub python: String,
}

impl Fixture {
    /// Write a `structural.toml` in `dir` pointing both built-ins at the
    /// mock.
    pub(super) fn write_config(&self, dir: &std::path::Path) {
        self.write_config_with_modes(dir, None, None);
    }

    /// As [`Fixture::write_config`], launching either server in a mock mode
    /// (`HOST_MOCK_PY`'s fourth argument).
    pub(super) fn write_config_with_modes(
        &self,
        dir: &std::path::Path,
        rust: Option<&str>,
        pyright: Option<&str>,
    ) {
        let command = |label: &str, mode: Option<&str>| {
            let mut argv = vec![
                self.python.clone(),
                self.script.display().to_string(),
                self.log.display().to_string(),
                label.to_string(),
            ];
            argv.extend(mode.map(str::to_string));
            let quoted: Vec<String> = argv.iter().map(|arg| format!("\"{arg}\"")).collect();
            format!("command = [{}]", quoted.join(", "))
        };
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            dir.join(crate::config::FILE_NAME),
            format!(
                "[servers.rust-analyzer]\n{}\n\n[servers.pyright]\n{}\n",
                command("rust-analyzer", rust),
                command("pyright", pyright)
            ),
        )
        .unwrap();
    }
}

/// `None` when there is no usable Python, which every test here reports as a
/// skip. Nothing else about the fixture can fail.
pub(super) fn fixture(name: &str) -> Option<Fixture> {
    let python = crate::test_support::python()?;
    let (dir, script) = scratch(name);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    // A source file, so file-scoped routing has something to match, and a
    // manifest so the tree says what it is.
    std::fs::write(dir.join("Cargo.toml"), "").unwrap();
    std::fs::write(dir.join("src/lib.rs"), "pub fn a() {}\n").unwrap();
    std::fs::write(dir.join("src/helper.py"), "def a():\n    pass\n").unwrap();
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
    Some(Fixture {
        root: dir,
        log,
        script,
        python,
    })
}

impl Fixture {
    /// One pid per launch of the mock, in the order they started.
    pub(super) fn launches(&self) -> Vec<u32> {
        self.events()
            .into_iter()
            .filter(|event| event.kind == "START")
            .map(|event| event.pid)
            .collect()
    }

    pub(super) fn source(&self) -> PathBuf {
        self.root.join("src/lib.rs")
    }

    /// The recorded events, in the order they reached the log: each launch,
    /// and the answer to its `initialize` when it came, each naming its
    /// server.
    pub(super) fn events(&self) -> Vec<Event> {
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
pub(super) struct Event {
    pub kind: String,
    pub server: String,
    pub pid: u32,
}

/// A fixture whose two servers differ: rust-analyzer's `initialize` stalls or
/// its command cannot be spawned, while pyright runs the mock normally. The
/// asymmetry is what makes independence observable.
pub(super) fn asymmetric_fixture(name: &str, rust: &str) -> Option<Fixture> {
    let python = crate::test_support::python()?;
    let (dir, script) = scratch(name);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("Cargo.toml"), "").unwrap();
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
    Some(Fixture {
        root: dir,
        log,
        script,
        python,
    })
}

/// A fixture whose sources and extra configuration are the case under
/// test: `sources` are written verbatim under the root, and `extra` is
/// appended to `structural.toml`.
///
/// Both built-ins run the mock, so what varies between these cases is only
/// what the tree says about itself and what the file says about the
/// servers — which is exactly what selection reads.
pub(super) fn selective_fixture(
    name: &str,
    sources: &[(&str, &str)],
    extra: &str,
) -> Option<Fixture> {
    let python = crate::test_support::python()?;
    let (dir, script) = scratch(name);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    for (path, contents) in sources {
        let file = dir.join(path);
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(file, contents).unwrap();
    }
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
            "[servers.rust-analyzer]\n{}\n\n[servers.pyright]\n{}\n{extra}",
            command("rust-analyzer"),
            command("pyright")
        ),
    )
    .unwrap();
    Some(Fixture {
        root: dir,
        log,
        script,
        python,
    })
}

/// A Rust tree whose *rust* candidates are two generic servers, so one plan
/// carries a ladder of two.
///
/// The shipped catalogue holds one server per language, so a fallback can
/// only be exercised against candidates supplied from outside it
/// (`docs/discovery.md`). `structural.toml` is that outside:
/// two entries declaring the same language are two candidates, and with
/// neither measured the rank falls through to name ascending — which is why
/// these are named so that `a-preferred` comes first and `b-alternative`
/// second. Both built-ins are disabled, so nothing else is in the tree's
/// way.
///
/// `preferred` says how the first candidate fails: `"serves"` runs the mock
/// normally, `"dies"` records its launch and exits before answering
/// `initialize`, and `"missing"` names a binary that cannot be spawned at
/// all. The three are the cases the ladder has to tell apart.
pub(super) fn ladder_fixture(name: &str, preferred: &str, alternative: &str) -> Option<Fixture> {
    let python = crate::test_support::python()?;
    let (dir, script) = scratch(name);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(dir.join("Cargo.toml"), "").unwrap();
    std::fs::write(dir.join("src/lib.rs"), "pub fn a() {}\n").unwrap();
    let log = dir.join("launches.log");
    let candidate = |label: &str, kind: &str| match kind {
        "missing" => format!("command = [\"no-such-server-{label}\"]"),
        kind => {
            let mut argv = vec![
                format!("\"{python}\""),
                format!("\"{}\"", script.display()),
                format!("\"{}\"", log.display()),
                format!("\"{label}\""),
            ];
            if kind == "dies" {
                argv.push("\"die\"".to_string());
            }
            format!("command = [{}]", argv.join(", "))
        }
    };
    std::fs::write(
        dir.join(crate::config::FILE_NAME),
        format!(
            "[servers.rust-analyzer]\ndisabled = true\n\n             [servers.pyright]\ndisabled = true\n\n             [servers.a-preferred]\n{}\nlanguages = [\"rust\"]\n\n             [servers.b-alternative]\n{}\nlanguages = [\"rust\"]\n",
            candidate("a-preferred", preferred),
            candidate("b-alternative", alternative),
        ),
    )
    .unwrap();
    Some(Fixture {
        root: dir,
        log,
        script,
        python,
    })
}

/// Which servers the mock recorded starting, sorted, after giving any
/// further launch a chance to appear.
///
/// The settle is what makes a negative assertion mean something: "pyright
/// never started" read immediately after `agent_started` would also pass
/// if it were merely slow. Eager warming waits for every launch it began
/// before returning, so this only has to outlast a launch that was never
/// planned — one that would have been started by the same call.
pub(super) fn servers_launched(fixture: &Fixture) -> Vec<String> {
    std::thread::sleep(std::time::Duration::from_millis(300));
    let mut servers: Vec<String> = fixture
        .events()
        .into_iter()
        .filter(|event| event.kind == "START")
        .map(|event| event.server)
        .collect();
    servers.sort();
    servers
}

/// Which servers the mock recorded starting, **in the order they started**,
/// after giving any further launch a chance to appear.
///
/// The ordered form, for the one claim that is about order: a ladder is
/// walked top down, and a sorted list could not tell a fallback from a
/// race. Safe to read as an order here because one slot's ladder is walked
/// by one launcher, serially.
pub(super) fn launch_order(fixture: &Fixture) -> Vec<String> {
    std::thread::sleep(std::time::Duration::from_millis(300));
    fixture
        .events()
        .into_iter()
        .filter(|event| event.kind == "START")
        .map(|event| event.server)
        .collect()
}

/// Wait for `n` launches to appear in the mock's log, up to a bound.
///
/// This observes a signal another thread produces — a pid that exists — and
/// never stands in for a claim that a server is ready; the bounded wait is
/// only so a failure reports rather than hangs.
pub(super) fn wait_for_launches(fixture: &Fixture, n: usize) -> Vec<u32> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let launches = fixture.launches();
        if launches.len() >= n || std::time::Instant::now() > deadline {
            return launches;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// Wait for `condition` to hold, up to a bound, and say whether it did.
///
/// For a state another thread produces — a line in the mock's log, a map
/// a shutdown has emptied — and never for a claim that something has
/// finished. The bound is generous because the machine is shared: it is
/// there so a failure reports rather than hangs, not to measure anything.
pub(super) fn wait_until(mut condition: impl FnMut() -> bool) -> bool {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        if condition() {
            return true;
        }
        if std::time::Instant::now() > deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

/// Whether `pid` names a live process. `ps` is the portable-enough probe:
/// teardown waits on the child, so a reaped pid is gone rather than a zombie.
pub(super) fn alive(pid: u32) -> bool {
    Command::new("ps")
        .args(["-p", &pid.to_string()])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}
