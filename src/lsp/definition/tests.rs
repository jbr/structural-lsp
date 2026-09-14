use super::*;
use lsp_types::{Position, SymbolKind};

fn symbol_info(name: &str, path: &str, line: u32) -> SymbolInformation {
    serde_json::from_value(json!({
        "name": name,
        "kind": SymbolKind::FUNCTION,
        "location": {
            "uri": format!("file://{path}"),
            "range": {
                "start": { "line": line, "character": 7 },
                "end": { "line": line, "character": 12 }
            }
        }
    }))
    .unwrap()
}

#[test]
fn one_exact_match_resolves() {
    let candidates = resolve(
        "make",
        vec![symbol_info("make", "/ws/src/lib.rs", 4)],
        Path::new("/ws"),
        None,
    );
    match candidates.resolution {
        Resolution::ExactOne(candidate) => {
            assert!(candidate.exact);
            assert_eq!(candidate.info.name, "make");
        }
        other => panic!("expected one exact match, got {other:?}"),
    }
    assert_eq!(candidates.out_of_scope, 0);
}

#[test]
fn duplicate_exact_names_are_ambiguous() {
    let candidates = resolve(
        "Config",
        vec![
            symbol_info("Config", "/ws/src/lib.rs", 3),
            symbol_info("Config", "/ws/src/other.rs", 9),
        ],
        Path::new("/ws"),
        None,
    );
    match candidates.resolution {
        Resolution::ExactMany(exact) => assert_eq!(exact.len(), 2),
        other => panic!("expected ambiguity, got {other:?}"),
    }
}

#[test]
fn fuzzy_only_matches_are_not_resolutions() {
    let candidates = resolve(
        "conf",
        vec![
            symbol_info("Config", "/ws/src/lib.rs", 3),
            symbol_info("configure", "/ws/src/lib.rs", 40),
        ],
        Path::new("/ws"),
        None,
    );
    match candidates.resolution {
        Resolution::FuzzyOnly(fuzzy) => {
            assert_eq!(fuzzy.len(), 2);
            assert!(fuzzy.iter().all(|c| !c.exact));
        }
        other => panic!("expected fuzzy-only, got {other:?}"),
    }
}

#[test]
fn nothing_reported_is_not_found() {
    let candidates = resolve("nope", vec![], Path::new("/ws"), None);
    assert!(matches!(candidates.resolution, Resolution::NotFound));
}

#[test]
fn scope_narrows_candidates_and_counts_exclusions() {
    let candidates = resolve(
        "make",
        vec![
            symbol_info("make", "/ws/src/lib.rs", 4),
            symbol_info("make", "/ws/src/gen/other.rs", 7),
        ],
        Path::new("/ws"),
        Some(Path::new("/ws/src/gen")),
    );
    match candidates.resolution {
        Resolution::ExactOne(candidate) => {
            let path = candidate_path(&candidate.info.location.uri).unwrap();
            assert!(path.starts_with("/ws/src/gen"));
        }
        other => panic!("expected the in-scope candidate, got {other:?}"),
    }
    assert_eq!(candidates.out_of_scope, 1);
}

#[test]
fn readiness_follows_true_false_true_transitions() {
    use crate::lsp::session::Readiness;
    use std::time::Duration;
    let t = |quiescent: bool| serde_json::json!({ "health": "ok", "quiescent": quiescent });
    // No status: pending stays pending.
    let readiness = Readiness::Pending.update(None, Duration::from_secs(1));
    assert!(matches!(readiness, Readiness::Pending));
    // First true starts an epoch.
    let readiness = readiness.update(Some(&t(true)), Duration::from_secs(2));
    let Readiness::Quiescent { after } = readiness else {
        panic!("expected quiescent after first true")
    };
    assert_eq!(after, Duration::from_secs(2));
    // Repeated true keeps the epoch (same `after`).
    let readiness = readiness.update(Some(&t(true)), Duration::from_secs(3));
    let Readiness::Quiescent { after } = readiness else {
        panic!("expected the epoch to survive a repeated true")
    };
    assert_eq!(after, Duration::from_secs(2));
    // A false ends the epoch — the latching bug fixed here.
    let readiness = readiness.update(Some(&t(false)), Duration::from_secs(4));
    assert!(matches!(readiness, Readiness::Pending));
    // The next true starts a NEW epoch, timestamped then.
    let readiness = readiness.update(Some(&t(true)), Duration::from_secs(5));
    let Readiness::Quiescent { after } = readiness else {
        panic!("expected a fresh epoch after false→true")
    };
    assert_eq!(after, Duration::from_secs(5));
    // A status without the flag changes nothing.
    let readiness = readiness.update(
        Some(&serde_json::json!({"health":"ok"})),
        Duration::from_secs(6),
    );
    let Readiness::Quiescent { after } = readiness else {
        panic!("expected the epoch to survive a status without the flag")
    };
    assert_eq!(after, Duration::from_secs(5));
}

#[test]
fn null_and_empty_definition_results_mean_none() {
    assert!(matches!(
        parse_definition_result(Value::Null).unwrap(),
        DefinitionSet::None
    ));
    assert!(matches!(
        parse_definition_result(json!([])).unwrap(),
        DefinitionSet::None
    ));
}

#[test]
fn single_location_parses_with_its_own_range() {
    let value = json!({
        "uri": "file:///ws/src/lib.rs",
        "range": { "start": {"line": 8, "character": 12}, "end": {"line": 8, "character": 18} }
    });
    match parse_definition_result(value).unwrap() {
        DefinitionSet::One(target) => {
            assert_eq!(target.uri.to_string(), "file:///ws/src/lib.rs");
            assert_eq!(
                target.selection_range.start,
                Position {
                    line: 8,
                    character: 12
                }
            );
            assert!(target.full_range.is_none());
            assert_eq!(target.provenance, NavigationProvenance::LocationRange);
        }
        other => panic!("expected one target, got {other:?}"),
    }
}

#[test]
fn location_arrays_and_links_parse_and_keep_provenance() {
    let value = json!([
        { "uri": "file:///ws/a.rs", "range": { "start": {"line": 1, "character": 0}, "end": {"line": 1, "character": 4} } },
        {
            "targetUri": "file:///ws/b.rs",
            "targetRange": { "start": {"line": 10, "character": 0}, "end": {"line": 14, "character": 0} },
            "targetSelectionRange": { "start": {"line": 10, "character": 7}, "end": {"line": 10, "character": 12} }
        }
    ]);
    match parse_definition_result(value).unwrap() {
        DefinitionSet::Many(targets) => {
            assert_eq!(targets.len(), 2);
            assert_eq!(targets[0].provenance, NavigationProvenance::LocationRange);
            assert_eq!(
                targets[1].provenance,
                NavigationProvenance::LinkSelectionRange
            );
            assert_eq!(
                targets[1].selection_range.start,
                Position {
                    line: 10,
                    character: 7
                }
            );
            assert_eq!(targets[1].full_range.as_ref().unwrap().start.line, 10);
        }
        other => panic!("expected many targets, got {other:?}"),
    }
}

#[test]
fn link_without_selection_range_falls_back_to_target_range_and_says_so() {
    let value = json!({
        "targetUri": "file:///ws/b.rs",
        "targetRange": { "start": {"line": 3, "character": 0}, "end": {"line": 5, "character": 0} }
    });
    match parse_definition_result(value).unwrap() {
        DefinitionSet::One(target) => {
            assert_eq!(target.provenance, NavigationProvenance::LinkTargetRange);
            assert_eq!(
                target.selection_range.start,
                Position {
                    line: 3,
                    character: 0
                }
            );
        }
        other => panic!("expected one target, got {other:?}"),
    }
}

#[test]
fn unrecognized_shapes_are_errors_not_guesses() {
    assert!(parse_definition_result(json!("a string")).is_err());
    assert!(parse_definition_result(json!(42)).is_err());
    assert!(parse_definition_result(json!([{"bogus": 1}])).is_err());
}

/// A scratch workspace with real files, for the canonicalization tests.
struct ScratchDir(std::path::PathBuf);

impl ScratchDir {
    fn new(tag: &str) -> Self {
        let base =
            std::env::temp_dir().join(format!("structural-scope-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(base.join("src")).unwrap();
        ScratchDir(base)
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn no_path_scope_still_excludes_candidates_outside_the_root() {
    let scratch = ScratchDir::new("root-filter");
    let outside = std::env::temp_dir().join(format!(
        "structural-scope-root-filter-outside-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&outside).unwrap();
    let inside = scratch.path().join("src/lib.rs");
    std::fs::write(&inside, "pub fn make() {}\n").unwrap();
    let outside_file = outside.join("lib.rs");
    std::fs::write(&outside_file, "pub fn make() {}\n").unwrap();

    let candidates = resolve(
        "make",
        vec![
            symbol_info("make", inside.to_str().unwrap(), 0),
            symbol_info("make", outside_file.to_str().unwrap(), 0),
        ],
        std::fs::canonicalize(scratch.path()).unwrap().as_path(),
        None,
    );
    match candidates.resolution {
        Resolution::ExactOne(candidate) => {
            assert_eq!(
                candidate.canonical_path.as_deref(),
                Some(std::fs::canonicalize(&inside).unwrap().as_path())
            );
        }
        other => panic!("expected the in-root candidate, got {other:?}"),
    }
    assert_eq!(
        candidates.out_of_root, 1,
        "the outside candidate is excluded"
    );
    assert_eq!(candidates.out_of_scope, 0);
    let _ = std::fs::remove_dir_all(&outside);
}

#[cfg(unix)]
#[test]
fn symlink_escape_is_excluded_even_when_the_link_sits_inside_the_root() {
    let scratch = ScratchDir::new("symlink-escape");
    let outside = std::env::temp_dir().join(format!(
        "structural-scope-escape-target-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&outside).unwrap();
    let outside_file = outside.join("lib.rs");
    std::fs::write(&outside_file, "pub fn make() {}\n").unwrap();
    let link = scratch.path().join("src/link.rs");
    std::os::unix::fs::symlink(&outside_file, &link).unwrap();

    // The URI points at the link, which sits inside the root; the
    // canonicalized file does not.
    let candidates = resolve(
        "make",
        vec![symbol_info("make", link.to_str().unwrap(), 0)],
        std::fs::canonicalize(scratch.path()).unwrap().as_path(),
        None,
    );
    assert!(matches!(candidates.resolution, Resolution::NotFound));
    assert_eq!(candidates.out_of_root, 1, "the symlink escape is excluded");
    let _ = std::fs::remove_dir_all(&outside);
}

#[cfg(unix)]
#[test]
fn symlink_to_another_in_root_file_is_kept_with_its_canonical_path() {
    let scratch = ScratchDir::new("symlink-inside");
    let real = scratch.path().join("src/real.rs");
    std::fs::write(&real, "pub fn make() {}\n").unwrap();
    let link = scratch.path().join("src/link.rs");
    std::os::unix::fs::symlink(&real, &link).unwrap();

    let candidates = resolve(
        "make",
        vec![symbol_info("make", link.to_str().unwrap(), 0)],
        std::fs::canonicalize(scratch.path()).unwrap().as_path(),
        None,
    );
    match candidates.resolution {
        Resolution::ExactOne(candidate) => {
            assert_eq!(
                candidate.canonical_path.as_deref(),
                Some(std::fs::canonicalize(&real).unwrap().as_path()),
                "the canonical (not the link) path is carried for refreshing"
            );
        }
        other => panic!("expected the in-root candidate, got {other:?}"),
    }
    assert_eq!(candidates.out_of_root, 0);
}
