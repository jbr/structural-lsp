//! What a language server actually answers, measured through structural's
//! own tools.
//!
//! This is a measurement harness, not a regression suite: it asserts only
//! that the probes ran, and prints a scorecard. Nothing here fails when a
//! server scores badly — a low score is the finding.
//!
//! The metric is deliberately narrow. `show`, `uses` and `action` need
//! symbol lookup, definitions whose ranges match the text, cross-file
//! references, containers from an outline, and a rename edit that can be
//! verified. A server's advertised capability list is a claim about all of
//! that; this asks instead. (The one place `src/` reads an advertised
//! capability is rename's `prepareProvider`, and everything else is checked
//! against the text it just read — `docs/correctness.md`.)
//!
//! Each result is **one observation on the server and version that ran**,
//! per `process/testing.md`: not validation, and not a general guarantee.
//! The version is recorded with the score for exactly that reason.
//!
//! Candidate commands come from `reference/servers-list/` — see
//! `scripts/reduce.py installed`, which prints the argv each source
//! recorded with a vote count. They are not invented here.
//!
//! ```sh
//! cargo test --test conformance -- --ignored --nocapture
//! ```

use mcplease::traits::Tool;
use std::path::{Path, PathBuf};
use std::process::Command;

mod report;

use report::{Outcome, Scorecard};

/// A server worth asking, with the argv the dataset recorded for it.
///
/// `builtin` marks a name `structural.toml` treats as an override of a
/// built-in adapter rather than a generic entry: the probe keeps the
/// built-in's readiness policy and capabilities, which is what that server
/// gets in production.
struct Candidate {
    name: &'static str,
    argv: &'static [&'static str],
    /// How to ask this server its version, when that is a different
    /// invocation than the server itself. `pyright-langserver --version`
    /// tries to open a connection and errors; the version lives on the
    /// sibling `pyright` binary.
    version_argv: &'static [&'static str],
    builtin: bool,
}

/// The Python candidates that are installed often enough to be worth the
/// harness. `ruff server` is deliberately absent: lsp-mode marks it an
/// add-on, and it serves diagnostics and formatting rather than the
/// navigation `show`/`uses`/`action` are built on.
const PYTHON: &[Candidate] = &[
    Candidate {
        name: "pyright",
        argv: &["pyright-langserver", "--stdio"],
        version_argv: &["pyright", "--version"],
        builtin: true,
    },
    Candidate {
        name: "basedpyright",
        argv: &["basedpyright-langserver", "--stdio"],
        version_argv: &["basedpyright", "--version"],
        builtin: false,
    },
    Candidate {
        name: "pylsp",
        argv: &["pylsp"],
        version_argv: &["pylsp", "--version"],
        builtin: false,
    },
    // Not installed here, but the argv is what the sources record, so the
    // probe picks them up on a machine that has them.
    Candidate {
        name: "jedi",
        argv: &["jedi-language-server"],
        version_argv: &["jedi-language-server", "--version"],
        builtin: false,
    },
    Candidate {
        name: "ty",
        argv: &["ty", "server"],
        version_argv: &["ty", "--version"],
        builtin: false,
    },
    Candidate {
        name: "pyrefly",
        argv: &["pyrefly", "lsp"],
        version_argv: &["pyrefly", "--version"],
        builtin: false,
    },
];

/// One question structural's tools need answered, and what a right answer
/// looks like. The name is what the scorecard prints.
struct Probe {
    name: &'static str,
    needs: &'static str,
    run: fn(&mut structural::session::Session) -> Outcome,
}

const PROBES: &[Probe] = &[
    Probe {
        name: "bare name",
        needs: "workspace/symbol",
        run: |state| {
            let answer = show(state, Some("choose"), None, None);
            Outcome::judge(&answer, |text| {
                text.contains("helper.py") && text.contains("def choose")
            })
        },
    },
    Probe {
        name: "file-scoped name",
        needs: "documentSymbol",
        run: |state| {
            let answer = show(state, Some("identity"), Some("helper.py"), None);
            Outcome::judge(&answer, |text| text.contains("def identity"))
        },
    },
    Probe {
        name: "qualified name",
        needs: "outline containers",
        run: |state| {
            let answer = show(state, Some("Config.doubled"), None, None);
            Outcome::judge(&answer, |text| text.contains("def doubled"))
        },
    },
    Probe {
        name: "cross-file uses",
        needs: "references",
        run: |state| {
            let answer = uses(state, "choose");
            // The definition is in helper.py and the only call is in
            // main.py: a server that reports just the definition has not
            // searched the project.
            Outcome::judge(&answer, |text| text.contains("main.py"))
        },
    },
    Probe {
        name: "anchor to a local",
        needs: "definition at a position",
        run: |state| {
            let answer = show(
                state,
                None,
                Some("main.py"),
                Some(("count = config.doubled()", "doubled")),
            );
            Outcome::judge(&answer, |text| text.contains("def doubled"))
        },
    },
    Probe {
        name: "rename preview",
        needs: "rename WorkspaceEdit",
        run: |state| {
            let answer = rename_preview(state, "choose", "pick");
            // Two files, because the call site is in the other one. A
            // rename that rewrites only the definition would break the
            // caller's code.
            Outcome::judge(&answer, |text| {
                text.contains("helper.py") && text.contains("main.py")
            })
        },
    },
    Probe {
        name: "bare name, warm",
        needs: "workspace/symbol after a file is open",
        run: |state| {
            // The same query as the first probe, run last. The probes share
            // one session, so by here every fixture file has been opened by
            // some earlier query. A `no` on the cold probe and a `yes` here
            // says the server's symbol index is empty until a file is
            // opened — a different catalogue fact from "cannot serve
            // workspace/symbol", and the same shape as
            // `backlog/bare-name-staleness.md`.
            let answer = show(state, Some("choose"), None, None);
            Outcome::judge(&answer, |text| {
                text.contains("helper.py") && text.contains("def choose")
            })
        },
    },
];

fn show(
    state: &mut structural::session::Session,
    symbol: Option<&str>,
    path: Option<&str>,
    anchor: Option<(&str, &str)>,
) -> anyhow::Result<String> {
    structural::tools::Show {
        symbol: symbol.map(str::to_string),
        path: path.map(str::to_string),
        anchor: anchor.map(|(text, _)| text.to_string()),
        focus: anchor.map(|(_, focus)| focus.to_string()),
        view: None,
    }
    .execute(state, &mcplease::types::RequestContext::default())
}

fn uses(state: &mut structural::session::Session, symbol: &str) -> anyhow::Result<String> {
    structural::tools::Uses {
        symbol: Some(symbol.to_string()),
        path: None,
        anchor: None,
        focus: None,
    }
    .execute(state, &mcplease::types::RequestContext::default())
}

fn rename_preview(
    state: &mut structural::session::Session,
    symbol: &str,
    new_name: &str,
) -> anyhow::Result<String> {
    structural::tools::Action {
        action: "rename".to_string(),
        symbol: Some(symbol.to_string()),
        path: None,
        anchor: None,
        focus: None,
        new_name: Some(new_name.to_string()),
        mode: None,
        digest: None,
    }
    .execute(state, &mcplease::types::RequestContext::default())
}

/// Whether a candidate is on `PATH`, and whether it will start.
///
/// Three states, not two, because the difference is the whole design
/// question behind walking a preference list. This machine has
/// `/usr/local/bin/pylsp`, a 2023 script whose shebang names a Python that
/// has since been removed: on `PATH`, and it cannot run. A lookup that
/// answered only "present" would put it at the head of the list forever.
///
/// Even `Runnable` is not availability — it means the process started, not
/// that it completed an LSP handshake. That is settled by the probes.
enum Reach {
    NotFound,
    Unrunnable(String),
    Runnable,
}

fn reach(argv: &[&str]) -> Reach {
    if !on_path(argv[0]) {
        return Reach::NotFound;
    }
    // Spawning the real argv is the honest check: a broken shebang, a
    // missing interpreter and a non-executable file all fail here, and none
    // of them fail a `PATH` lookup.
    match Command::new(argv[0])
        .args(&argv[1..])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(mut child) => {
            let _ = child.kill();
            let _ = child.wait();
            Reach::Runnable
        }
        Err(error) => Reach::Unrunnable(error.to_string()),
    }
}

/// Whether a bare name resolves against `PATH`, without running it.
fn on_path(program: &str) -> bool {
    if program.contains('/') {
        return Path::new(program).exists();
    }
    std::env::var_os("PATH")
        .map(|paths| std::env::split_paths(&paths).any(|dir| dir.join(program).exists()))
        .unwrap_or(false)
}

/// What the server reports for its version, recorded with the score.
///
/// One observation is only meaningful against a named version
/// (process/testing.md), so a blank here is a gap in the record rather than
/// a detail. A line that reads as an error or a stack trace is dropped:
/// several servers answer `--version` by trying to start.
fn version(argv: &[&str]) -> String {
    let Ok(output) = Command::new(argv[0]).args(&argv[1..]).output() else {
        return String::new();
    };
    let text = if output.stdout.is_empty() {
        output.stderr
    } else {
        output.stdout
    };
    let line = String::from_utf8_lossy(&text)
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string();
    if line.starts_with("Error") || line.starts_with('/') || line.len() > 80 {
        String::new()
    } else {
        line
    }
}

fn quoted(what: &str) -> String {
    format!("\"{}\"", what.replace('\\', "\\\\").replace('"', "\\\""))
}

/// A scratch copy of the Python fixture with exactly one server configured.
///
/// The other built-ins are pointed at commands that do not exist: built-ins
/// are on by default, and a second server answering the same names would
/// make every probe ambiguous rather than measuring the one under test.
fn scratch_root(candidate: &Candidate) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "structural-conformance-{}-{}",
        candidate.name,
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let fixtures = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures/python"));
    for entry in std::fs::read_dir(fixtures).unwrap() {
        let entry = entry.unwrap();
        std::fs::copy(entry.path(), root.join(entry.file_name())).unwrap();
    }

    let command = candidate
        .argv
        .iter()
        .map(|part| quoted(part))
        .collect::<Vec<_>>()
        .join(", ");
    let mut toml = String::new();
    if candidate.builtin {
        toml.push_str(&format!(
            "[servers.{}]\ncommand = [{command}]\n\n",
            candidate.name
        ));
    } else {
        toml.push_str(&format!(
            "[servers.{}]\ncommand = [{command}]\nlanguages = [\"python\"]\n\n",
            candidate.name
        ));
        toml.push_str("[servers.pyright]\ncommand = [\"/nonexistent/pyright\"]\n\n");
    }
    toml.push_str("[servers.rust-analyzer]\ncommand = [\"/nonexistent/rust-analyzer\"]\n");
    std::fs::write(root.join("structural.toml"), toml).unwrap();
    std::fs::canonicalize(root).unwrap()
}

#[test]
#[ignore = "spawns every installed Python server; run explicitly with -- --ignored --nocapture"]
fn python_servers_answer_structurals_questions() {
    let mut scorecard = Scorecard::new("python", PROBES.iter().map(|p| p.name).collect());
    let mut probed = 0;

    for candidate in PYTHON {
        match reach(candidate.argv) {
            Reach::NotFound => {
                scorecard.absent(candidate.name, candidate.argv.join(" "), None);
                continue;
            }
            Reach::Unrunnable(why) => {
                scorecard.absent(candidate.name, candidate.argv.join(" "), Some(why));
                continue;
            }
            Reach::Runnable => {}
        }
        probed += 1;
        let root = scratch_root(candidate);
        let mut state = structural::session::Session::new(root.clone());
        let outcomes = PROBES.iter().map(|probe| (probe.run)(&mut state)).collect();
        scorecard.scored(
            candidate.name,
            candidate.argv.join(" "),
            version(candidate.version_argv),
            outcomes,
        );
        drop(state);
        let _ = std::fs::remove_dir_all(&root);
    }

    print!(
        "{}",
        scorecard.render(PROBES.iter().map(|p| p.needs).collect())
    );

    // The harness is what is asserted, not the scores: a server that
    // answers nothing is a finding to read, not a failure. Only a run that
    // probed no server at all means the harness itself did not work.
    assert!(
        probed > 0,
        "no candidate was on PATH; the probe measured nothing"
    );
}
