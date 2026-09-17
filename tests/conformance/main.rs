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
//! The version is recorded with the score for exactly that reason, and so
//! is the fixture revision, because the tree is materialized from
//! `src/fixtures.rs` rather than resident in the repository.
//!
//! Candidate commands come from `reference/servers-list/` — see
//! `scripts/reduce.py installed`, which prints the argv each source
//! recorded with a vote count. They are not invented here.
//!
//! ```sh
//! cargo test --test conformance -- --ignored --nocapture
//! ```

use std::path::{Path, PathBuf};
use std::process::Command;

mod languages;
mod probe;
mod report;

use languages::{go::GO, python::PYTHON, rust::RUST};
use probe::{Candidate, Language};
use report::Scorecard;

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

/// The language's fixture tree with exactly one server configured.
///
/// The other built-ins are pointed at commands that do not exist: built-ins
/// are on by default, and a second server answering the same names would
/// make every probe ambiguous rather than measuring the one under test.
fn scratch_root(language: &Language, candidate: &Candidate) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "structural-conformance-{}-{}-{}",
        language.name,
        candidate.name,
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    structural::fixtures::get(language.fixture)
        .expect("a known fixture")
        .materialize(&root)
        .unwrap();
    for (path, text) in language.extra_files {
        let target = root.join(path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(target, text).unwrap();
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
            "[servers.{}]\ncommand = [{command}]\nlanguages = [\"{}\"]\n\n",
            candidate.name, language.name
        ));
    }
    for builtin in ["pyright", "rust-analyzer"] {
        if builtin != candidate.name {
            toml.push_str(&format!(
                "[servers.{builtin}]\ncommand = [\"/nonexistent/{builtin}\"]\n\n"
            ));
        }
    }
    std::fs::write(root.join("structural.toml"), toml).unwrap();
    std::fs::canonicalize(root).unwrap()
}

/// Ask every installed candidate of one language every probe, and print the
/// scorecard.
///
/// Asserts only that something was probed. A server that answers nothing is
/// a finding to read; a run that probed no server at all means the harness
/// did not work.
fn measure(language: &Language) {
    let fixture = structural::fixtures::get(language.fixture).expect("a known fixture");
    let mut scorecard = Scorecard::new(
        language.name,
        language.probes.iter().map(|probe| probe.name).collect(),
        fixture.revision(),
    );
    let mut probed = 0;

    for candidate in language.candidates {
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
        let root = scratch_root(language, candidate);
        let mut state = structural::session::Session::new(root.clone());
        let outcomes = language
            .probes
            .iter()
            .map(|probe| (probe.run)(&mut state))
            .collect();
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
        scorecard.render(language.probes.iter().map(|probe| probe.needs).collect())
    );

    assert!(
        probed > 0,
        "no {} candidate was on PATH; the probe measured nothing",
        language.name
    );
}

#[test]
#[ignore = "spawns every installed Python server; run explicitly with -- --ignored --nocapture"]
fn python_servers_answer_structurals_questions() {
    measure(&PYTHON);
}

#[test]
#[ignore = "spawns rust-analyzer; run explicitly with -- --ignored --nocapture"]
fn rust_servers_answer_structurals_questions() {
    measure(&RUST);
}

#[test]
#[ignore = "spawns gopls; run explicitly with -- --ignored --nocapture"]
fn go_servers_answer_structurals_questions() {
    measure(&GO);
}
