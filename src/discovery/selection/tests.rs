//! Plans on constructed trees with constructed catalogues.
//!
//! Every case here is one of `backlog/discovery/selection.md`'s Done-when
//! clauses made able to fail. Nothing starts a process: the assertion is
//! the returned value, which is the point of the layer.
//!
//! The catalogues are fixtures rather than the shipped one, deliberately.
//! The shipped catalogue has one entry per language and no scores until
//! `catalogue-synthesis.md` produces them, so ordering and fallback could
//! not be exercised against it — and that is what the plan means when it
//! says the mechanism is what is under test.

use super::*;
use crate::discovery::catalogue::{Measurement, Provenance, Score, Serves};
use crate::discovery::evidence::Sources;
use crate::discovery::roots::Ancestry;
use std::path::PathBuf;

/// A scratch tree: directories to create, then files to write (empty).
fn tree(name: &str, directories: &[&str], files: &[&str]) -> PathBuf {
    let root =
        std::env::temp_dir().join(format!("structural-selection-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    for directory in directories {
        std::fs::create_dir_all(root.join(directory)).unwrap();
    }
    for file in files {
        let path = root.join(file);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, "").unwrap();
    }
    std::fs::canonicalize(root).unwrap()
}

fn entry(name: &str, language: &str, score: Option<u8>) -> Entry {
    Entry {
        name: name.to_string(),
        command: vec![name.to_string()],
        serves: vec![Serves::known(language).expect("test language is known")],
        score: score.map(|answered| Score {
            answered,
            probes: 7,
            measurement: Measurement {
                server_version: "1.0.0".to_string(),
                structural_revision: "abc1234".to_string(),
                fixture_revision: "fixture-1".to_string(),
                date: "2026-09-18".to_string(),
            },
        }),
        consensus: 0,
        provenance: Provenance {
            source: "test".to_string(),
            extensions: Confidence::Exact,
        },
    }
}

fn servers(selection: &Selection) -> Vec<String> {
    selection.plans.iter().map(|p| p.server.clone()).collect()
}

fn plan_for<'a>(selection: &'a Selection, language: &str) -> &'a Plan {
    selection
        .plans
        .iter()
        .find(|p| p.language == language)
        .unwrap_or_else(|| panic!("no plan for {language}"))
}

#[test]
fn irrelevant_servers_are_not_planned() {
    // The defect this unit exists to close: today every configured server
    // warms at `agent_started`, so a Rust server starts in a Python-only
    // project. Here it is absent from the value, which is what makes the
    // launch-log assertion in milestone B possible at all.
    let root = tree("python-only", &["src"], &["pyproject.toml", "src/app.py"]);
    let evidence = Evidence::gather(&root);
    let catalogue = Catalogue::new(vec![
        entry("pyright", "python", Some(6)),
        entry("rust-analyzer", "rust", Some(7)),
    ]);

    let selection = select(&evidence, &catalogue, &Overrides::new());

    assert_eq!(servers(&selection), vec!["pyright"]);
    assert_eq!(
        selection.declined_reason("rust-analyzer"),
        Some(&DeclinedReason::NoEvidence {
            languages: vec!["rust".to_string()]
        }),
        "a higher score does not make a server relevant to a tree without its language"
    );
}

#[test]
fn the_highest_scoring_candidate_wins_and_the_rest_are_alternatives() {
    let root = tree("scored", &["src"], &["pyproject.toml", "src/app.py"]);
    let evidence = Evidence::gather(&root);
    let catalogue = Catalogue::new(vec![
        entry("capable", "python", Some(6)),
        entry("mediocre", "python", Some(3)),
        entry("poor", "python", Some(1)),
    ]);

    let selection = select(&evidence, &catalogue, &Overrides::new());
    let plan = plan_for(&selection, "python");

    assert_eq!(plan.server, "capable");
    assert_eq!(
        plan.alternatives,
        vec!["mediocre", "poor"],
        "in rank order, which is what milestone C walks"
    );
    assert!(plan.why.contains(&Reason::HighestScore {
        answered: 6,
        probes: 7
    }));
    assert_eq!(
        selection.declined_reason("mediocre"),
        Some(&DeclinedReason::Outranked {
            by: vec!["capable".to_string()]
        })
    );
}

#[test]
fn one_candidate_did_not_beat_anything() {
    // The distinction that matters when a machine has one server per
    // language, which is the common case: "chosen because it won" and
    // "chosen because there was no one else" are different claims, and
    // only the second is defensible here.
    let root = tree("lone", &["src"], &["pyproject.toml", "src/app.py"]);
    let evidence = Evidence::gather(&root);
    let catalogue = Catalogue::new(vec![entry("pyright", "python", Some(6))]);

    let selection = select(&evidence, &catalogue, &Overrides::new());
    let plan = plan_for(&selection, "python");

    assert!(plan.why.contains(&Reason::OnlyCandidate));
    assert!(
        !plan.why.iter().any(|r| matches!(r, Reason::HighestScore { .. })),
        "it outranked nothing, so it must not claim to have"
    );
    assert!(plan.alternatives.is_empty());
}

#[test]
fn an_unmeasured_winner_says_so() {
    let root = tree("unmeasured", &["src"], &["pyproject.toml", "src/app.py"]);
    let evidence = Evidence::gather(&root);
    let catalogue = Catalogue::new(vec![entry("pyright", "python", None)]);

    let selection = select(&evidence, &catalogue, &Overrides::new());

    assert!(plan_for(&selection, "python").why.contains(&Reason::Unmeasured));
}

#[test]
fn plans_are_ordered_by_a_stated_rule_not_by_the_catalogue() {
    // Determinism asserted against the stated order — language ascending,
    // then server — rather than against a second run, which can agree
    // while the property is false (`warm::first_served_file`).
    let root = tree(
        "polyglot",
        &["src", "pysrc"],
        &["Cargo.toml", "pyproject.toml", "src/main.rs", "pysrc/app.py"],
    );
    let evidence = Evidence::gather(&root);

    let forward = Catalogue::new(vec![
        entry("rust-analyzer", "rust", Some(7)),
        entry("pyright", "python", Some(6)),
    ]);
    let reversed = Catalogue::new(vec![
        entry("pyright", "python", Some(6)),
        entry("rust-analyzer", "rust", Some(7)),
    ]);

    let expected = vec!["pyright".to_string(), "rust-analyzer".to_string()];
    assert_eq!(
        servers(&select(&evidence, &forward, &Overrides::new())),
        expected,
        "python before rust: languages ascending, not insertion order"
    );
    assert_eq!(
        servers(&select(&evidence, &reversed, &Overrides::new())),
        expected
    );
}

#[test]
fn an_override_can_disable_a_server_which_configuration_cannot_do_today() {
    // `Config::adapters` always emits both built-ins; there is no way to
    // say "not in this project". This is that capability.
    let root = tree("disabled", &["src"], &["Cargo.toml", "src/main.rs"]);
    let evidence = Evidence::gather(&root);
    let catalogue = Catalogue::new(vec![entry("rust-analyzer", "rust", Some(7))]);

    let selection = select(
        &evidence,
        &catalogue,
        &Overrides::new().disable("rust-analyzer"),
    );

    assert!(selection.plans.is_empty());
    assert_eq!(
        selection.declined_reason("rust-analyzer"),
        Some(&DeclinedReason::Disabled)
    );
}

#[test]
fn an_override_beats_the_catalogue_even_when_the_catalogue_scored_better() {
    // Precedence, stated: override, then catalogue, then a guess from
    // evidence. An added entry is unmeasured and still wins, because the
    // operator naming a server is not a guess.
    let root = tree("override-wins", &["src"], &["pyproject.toml", "src/app.py"]);
    let evidence = Evidence::gather(&root);
    let catalogue = Catalogue::new(vec![entry("pyright", "python", Some(7))]);

    let mut mine = entry("pyright", "python", None);
    mine.command = vec!["/opt/mine/pyright".to_string()];
    let selection = select(&evidence, &catalogue, &Overrides::new().configure(mine));
    let plan = plan_for(&selection, "python");

    assert_eq!(plan.command, vec!["/opt/mine/pyright"]);
    assert!(plan.why.contains(&Reason::Configured));
}

#[test]
fn an_override_can_add_a_server_the_catalogue_does_not_know() {
    let root = tree("added", &["src"], &["Cargo.toml", "src/main.rs"]);
    let evidence = Evidence::gather(&root);
    let catalogue = Catalogue::new(vec![]);

    let selection = select(
        &evidence,
        &catalogue,
        &Overrides::new().configure(entry("some-rust-server", "rust", None)),
    );

    assert_eq!(servers(&selection), vec!["some-rust-server"]);
}

#[test]
fn every_plan_says_why_and_the_reasons_match_the_evidence() {
    // "Explainable" fails when a plan is right for the wrong reason, so
    // the reasons are asserted as data against the fixture that produced
    // them rather than merely being non-empty.
    let root = tree(
        "reasons",
        &["src"],
        &["Cargo.toml", "src/main.rs", "src/lib.rs"],
    );
    let evidence = Evidence::gather(&root);
    let catalogue = Catalogue::new(vec![entry("rust-analyzer", "rust", Some(7))]);

    let selection = select(&evidence, &catalogue, &Overrides::new());
    let plan = plan_for(&selection, "rust");

    assert_eq!(
        plan.why,
        vec![
            Reason::Marker {
                file: "Cargo.toml".to_string(),
                language: "rust".to_string(),
            },
            Reason::Sources {
                extension: "rs".to_string(),
                count: 2,
            },
            Reason::OnlyCandidate,
        ]
    );
    assert_eq!(
        plan.explain(),
        "rust-analyzer for rust: `Cargo.toml` implies rust; \
         2 `.rs` files in the tree; the only candidate for this language"
    );
}

#[test]
fn an_inferred_extension_is_not_allowed_to_pass_as_a_fact() {
    // The dataset's weakest link is the Emacs sources' extensions, many
    // matched by name against another editor's table. A plan chosen on
    // one says so.
    let root = tree("inferred", &["src"], &["src/app.py"]);
    let evidence = Evidence::gather(&root);
    let mut guessed = entry("guessy", "python", Some(4));
    guessed.provenance.extensions = Confidence::Inferred;
    let catalogue = Catalogue::new(vec![guessed]);

    let selection = select(&evidence, &catalogue, &Overrides::new());

    assert!(
        plan_for(&selection, "python")
            .why
            .contains(&Reason::ExtensionsInferred)
    );
}

#[test]
fn sources_alone_are_evidence_when_there_is_no_marker() {
    // A directory of Python with no `pyproject.toml`. The root falls back
    // to the working directory and the language is still seen.
    let root = tree("no-manifest", &[], &["a.py", "b.py"]);
    let evidence = Evidence::gather(&root);
    let catalogue = Catalogue::new(vec![entry("pyright", "python", Some(6))]);

    let selection = select(&evidence, &catalogue, &Overrides::new());
    let plan = plan_for(&selection, "python");

    assert_eq!(plan.root, root);
    assert_eq!(
        plan.why.first(),
        Some(&Reason::Sources {
            extension: "py".to_string(),
            count: 2
        })
    );
}

#[test]
fn a_marker_for_a_language_no_entry_serves_plans_nothing() {
    // Evidence without a server is not a decline — there is no server to
    // decline — and it is not an error here either. Telling the operator
    // is `operator-diagnostics.md`.
    let root = tree("unserved", &[], &["go.mod", "main.go"]);
    let evidence = Evidence::gather(&root);
    let catalogue = Catalogue::new(vec![entry("pyright", "python", Some(6))]);

    let selection = select(&evidence, &catalogue, &Overrides::new());

    assert!(selection.plans.is_empty());
    assert_eq!(
        selection.declined_reason("pyright"),
        Some(&DeclinedReason::NoEvidence {
            languages: vec!["python".to_string()]
        })
    );
}

#[test]
fn the_plan_root_is_the_project_not_the_working_directory() {
    // The sharing point: two agents in sibling members of one cargo
    // workspace plan the same root, which is what lets them share one
    // server in milestone B. Narrowing stays the caller's cwd and is not
    // this value's job.
    let root = tree(
        "members",
        &[".git", "crates/alpha/src", "crates/beta/src"],
        &[
            "Cargo.toml",
            "crates/alpha/Cargo.toml",
            "crates/alpha/src/lib.rs",
            "crates/beta/Cargo.toml",
            "crates/beta/src/lib.rs",
        ],
    );
    let catalogue = Catalogue::new(vec![entry("rust-analyzer", "rust", Some(7))]);

    let alpha = select(
        &Evidence::gather(&root.join("crates/alpha")),
        &catalogue,
        &Overrides::new(),
    );
    let beta = select(
        &Evidence::gather(&root.join("crates/beta")),
        &catalogue,
        &Overrides::new(),
    );

    assert_eq!(plan_for(&alpha, "rust").root, root);
    assert_eq!(plan_for(&beta, "rust").root, root);
}

#[test]
fn selection_is_a_function_of_its_inputs_alone() {
    // Purity, stated as a test: the same tree and catalogue give an equal
    // value. This does not prove determinism — the stated-order tests do
    // that — but it catches a hidden dependency on time, environment, or
    // a process-global.
    let root = tree("pure", &["src"], &["Cargo.toml", "src/main.rs"]);
    let evidence = Evidence::gather(&root);
    let catalogue = Catalogue::new(vec![entry("rust-analyzer", "rust", Some(7))]);

    assert_eq!(
        select(&evidence, &catalogue, &Overrides::new()),
        select(&evidence, &catalogue, &Overrides::new())
    );
}

#[test]
fn evidence_can_be_supplied_without_a_filesystem() {
    // Selection takes evidence, not a path, so a case that is awkward to
    // build as a tree is still a test. Here: a language present only as
    // sources, with a count no fixture needs to create files for.
    let ancestry = Ancestry::gather(&PathBuf::from("/nonexistent/project"));
    let mut sources = Sources::default();
    sources.by_extension.insert("rs".to_string(), 4_000);
    sources.files_seen = 4_000;
    let evidence = Evidence::from_parts(ancestry, sources);
    let catalogue = Catalogue::new(vec![entry("rust-analyzer", "rust", Some(7))]);

    let selection = select(&evidence, &catalogue, &Overrides::new());

    assert!(selection.proposes("rust-analyzer"));
    assert!(plan_for(&selection, "rust").why.contains(&Reason::Sources {
        extension: "rs".to_string(),
        count: 4_000
    }));
}
