use super::*;
use crate::lsp::positions::Encoding;
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
        "rust-analyzer",
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
        "rust-analyzer",
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
        "rust-analyzer",
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

/// ccls advertises `int choose(bool flag)` in its index for a definition
/// the file spells `choose`; elp advertises `choose/1`. Both mention the
/// name once as a whole token, and nothing else matched it, so both
/// nominate it — under the identifier, which is what every range check is
/// made against (`../../../journal/2026-09-19-a-name-is-what-the-range-says.md`).
#[test]
fn a_decorated_name_nominates_when_nothing_matched_the_bare_one() {
    let candidates = resolve(
        "choose",
        "ccls",
        vec![symbol_info("int choose(bool flag)", "/ws/helper.cpp", 6)],
        Path::new("/ws"),
        None,
    );
    match candidates.resolution {
        Resolution::ExactOne(candidate) => {
            assert!(candidate.exact);
            assert_eq!(
                candidate.info.name, "choose",
                "the identity is the identifier, not the server's spelling of it"
            );
        }
        other => panic!("expected the decorated candidate, got {other:?}"),
    }
}

/// A candidate that advertises the bare name settles it; a decoration is
/// the fallback, never a second exact match beside one.
#[test]
fn a_plain_name_keeps_a_decorated_one_out_of_the_exact_set() {
    let candidates = resolve(
        "choose",
        "ccls",
        vec![
            symbol_info("choose", "/ws/helper.cpp", 6),
            symbol_info("int choose(bool flag)", "/ws/helper.hpp", 9),
        ],
        Path::new("/ws"),
        None,
    );
    match candidates.resolution {
        Resolution::ExactOne(candidate) => {
            assert_eq!(
                candidate.canonical_path.as_deref(),
                Some(Path::new("/ws/helper.cpp"))
            );
        }
        other => panic!("expected one exact match, got {other:?}"),
    }
}

#[test]
fn a_decoration_must_mention_the_name_as_a_whole_token() {
    assert!(decorates("choose/1", "choose"));
    assert!(decorates("int choose(bool flag)", "choose"));
    assert!(decorates("(Config).Doubled", "Doubled"));
    assert!(!decorates("choose", "choose"), "a name is not a decoration");
    assert!(!decorates("chooser/1", "choose"), "no token boundary");
    assert!(!decorates("choose_all", "choose"));
    assert!(
        !decorates("choose(choose)", "choose"),
        "two occurrences say nothing about which one is meant"
    );
}

#[test]
fn nothing_reported_is_not_found() {
    let candidates = resolve("nope", "rust-analyzer", vec![], Path::new("/ws"), None);
    assert!(matches!(candidates.resolution, Resolution::NotFound));
}

#[test]
fn scope_narrows_candidates_and_counts_exclusions() {
    let candidates = resolve(
        "make",
        "rust-analyzer",
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
        "rust-analyzer",
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
        "rust-analyzer",
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
        "rust-analyzer",
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

#[test]
fn verify_name_at_accepts_the_name_and_empty_ranges_only() {
    let text = "pub struct Config {\n    pub limit: usize,\n}\n";
    let r = |sl, sc, el, ec| lsp_types::Range {
        start: lsp_types::Position::new(sl, sc),
        end: lsp_types::Position::new(el, ec),
    };
    verify_name_at(text, r(0, 11, 0, 17), "Config", Encoding::Utf16).unwrap();
    // A module's definition is the top of its file: nothing to check.
    verify_name_at(text, r(0, 0, 0, 0), "anything", Encoding::Utf16).unwrap();
    let error = verify_name_at(text, r(0, 11, 0, 17), "Settings", Encoding::Utf16).unwrap_err();
    assert!(error.to_string().contains("found `Config`"), "{error}");
    assert!(error.to_string().contains("1:12"), "{error}");
    let error = verify_name_at(text, r(0, 4, 0, 10), "Config", Encoding::Utf16).unwrap_err();
    assert!(error.to_string().contains("found `struct`"), "{error}");
    assert!(
        verify_name_at(text, r(9, 0, 9, 6), "Config", Encoding::Utf16).is_err(),
        "outside the file"
    );
    assert!(
        verify_name_at(text, r(0, 17, 0, 11), "Config", Encoding::Utf16).is_err(),
        "inverted"
    );
}

/// A range wider than the name resolves, and answers with the name's own
/// position — `SymbolInformation.location.range` is the range enclosing the
/// symbol, so a whole declaration is a conforming answer and a
/// `definition` request still has to be sent at the name
/// (`backlog/symbol-range-convention.md`).
#[test]
fn locate_name_at_finds_the_name_inside_a_whole_declaration_range() {
    let text = "pub struct Config {\n    pub limit: usize,\n}\n";
    let r = |sl, sc, el, ec| lsp_types::Range {
        start: lsp_types::Position::new(sl, sc),
        end: lsp_types::Position::new(el, ec),
    };
    // The tight range: the position is the range's own start.
    match locate_name_at(text, r(0, 11, 0, 17), "Config", Encoding::Utf16).unwrap() {
        NameLocation::At(position) => assert_eq!(position, lsp_types::Position::new(0, 11)),
        NameLocation::Unnamed => panic!("the range names Config"),
    }
    // The declaration line: the same position, found inside it.
    match locate_name_at(text, r(0, 0, 0, 19), "Config", Encoding::Utf16).unwrap() {
        NameLocation::At(position) => assert_eq!(position, lsp_types::Position::new(0, 11)),
        NameLocation::Unnamed => panic!("the range contains Config"),
    }
    // A range that does not hold the name at all is the disagreement the
    // check exists for, and it stays an error.
    assert!(locate_name_at(text, r(1, 4, 1, 20), "Config", Encoding::Utf16).is_err());
}

#[test]
fn locate_name_at_requires_a_token_boundary_and_refuses_two_occurrences() {
    let r = |sl, sc, el, ec| lsp_types::Range {
        start: lsp_types::Position::new(sl, sc),
        end: lsp_types::Position::new(el, ec),
    };
    // `choose` inside `chooser` is a different name, so the wider range
    // holds no occurrence of it.
    let text = "def chooser(flag):\n    return 1\n";
    let error = locate_name_at(text, r(0, 0, 0, 18), "choose", Encoding::Utf16).unwrap_err();
    assert!(
        error.to_string().contains("found `def chooser(flag):`"),
        "{error}"
    );

    // Two occurrences in one range: which one the server meant is not
    // decidable, and nothing ambiguous is silently picked.
    let text = "def choose(choose: int):\n    return 1\n";
    let error = locate_name_at(text, r(0, 0, 0, 24), "choose", Encoding::Utf16).unwrap_err();
    assert!(error.to_string().contains("more than once"), "{error}");
}

fn candidate(server: &str, name: &str, path: &str, line: u32) -> Candidate {
    Candidate {
        server: server.to_string(),
        canonical_path: Some(std::path::PathBuf::from(path)),
        info: symbol_info(name, path, line),
        exact: true,
    }
}

fn target_at(path: &str, line: u32) -> DefinitionTarget {
    DefinitionTarget {
        uri: format!("file://{path}").parse().unwrap(),
        selection_range: lsp_types::Range {
            start: lsp_types::Position::new(line, 4),
            end: lsp_types::Position::new(line, 10),
        },
        full_range: None,
        provenance: NavigationProvenance::LocationRange,
    }
}

#[test]
fn candidates_with_one_definition_target_are_one_identity() {
    // Two hits for `Config` (a declaration and, say, a re-export the server
    // did not exclude) both navigate to the declaration: one identity, both
    // candidates kept, the first one first.
    let found = vec![
        (
            candidate("rust-analyzer", "Config", "/ws/src/lib.rs", 4),
            DefinitionSet::One(target_at("/ws/src/lib.rs", 4)),
        ),
        (
            candidate("rust-analyzer", "Config", "/ws/src/tests.rs", 9),
            DefinitionSet::One(target_at("/ws/src/lib.rs", 4)),
        ),
    ];
    let identities = group_by_target(found);
    assert_eq!(identities.len(), 1);
    assert_eq!(identities[0].candidates.len(), 2);
    assert_eq!(
        identities[0].candidates[0].info.location.range.start.line,
        4
    );
    assert!(identities[0].target.is_some());
}

#[test]
fn distinct_targets_stay_distinct_and_none_is_listed_not_dropped() {
    let found = vec![
        (
            candidate("rust-analyzer", "Server", "/ws/src/spike.rs", 30),
            DefinitionSet::One(target_at("/ws/src/spike.rs", 30)),
        ),
        (
            candidate("rust-analyzer", "Server", "/ws/tests/stdio.rs", 7),
            DefinitionSet::One(target_at("/ws/tests/stdio.rs", 7)),
        ),
        (
            candidate("pyright", "Server", "/ws/py/server.py", 1),
            DefinitionSet::None,
        ),
        // The same place, reached from another server, is not the same
        // identity: identity is per server.
        (
            candidate("gopls", "Server", "/ws/src/spike.rs", 30),
            DefinitionSet::Many(vec![
                target_at("/ws/src/spike.rs", 30),
                target_at("/ws/src/other.rs", 2),
            ]),
        ),
    ];
    let identities = group_by_target(found);
    let summary: Vec<(String, Option<u32>)> = identities
        .iter()
        .map(|identity| {
            (
                identity.server.clone(),
                identity
                    .target
                    .as_ref()
                    .map(|target| target.selection_range.start.line),
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            ("rust-analyzer".to_string(), Some(30)),
            ("rust-analyzer".to_string(), Some(7)),
            ("pyright".to_string(), None),
            ("gopls".to_string(), Some(30)),
            ("gopls".to_string(), Some(2)),
        ]
    );
    assert_eq!(
        identities[2].kind(),
        Some(identities[2].candidates[0].info.kind)
    );
    assert_eq!(identities[2].name(), "Server");
}
