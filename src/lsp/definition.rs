//! Symbol resolution and definition-result normalization, as pure functions.
//!
//! No I/O and no language server: [`resolve`] partitions `workspace/symbol`
//! candidates into exact/fuzzy/ambiguous, and [`parse_definition_result`]
//! defensively normalizes what `textDocument/definition` returns (`Location`,
//! `Location[]`, `LocationLink[]`, `LocationLink`, `null`, `[]` — every shape
//! the spec allows). The MCP tool layer composes these with the session.

use anyhow::{Context, Result, bail};
use lsp_types::{Location, Range, SymbolInformation, Uri};
use serde_json::Value;
#[cfg(test)]
use serde_json::json;
use std::path::Path;

/// One `workspace/symbol` candidate as the server reported it, with the
/// canonicalized file path used for scope filtering and refreshing.
#[derive(Clone, Debug)]
pub struct Candidate {
    pub info: SymbolInformation,
    /// The candidate's file canonicalized when possible. Symlinks resolve to
    /// their real location before any scope check, so a link out of the root
    /// is excluded rather than smuggled in. `None` when the URI is not a
    /// `file:` path or the file cannot be canonicalized (missing or broken
    /// link) — such a candidate is kept only for reporting and cannot be
    /// refreshed.
    pub canonical_path: Option<std::path::PathBuf>,
    /// Whether the candidate's name matches the query exactly. Only exact
    /// matches are resolvable; fuzzy matches are shown as possible matches,
    /// never silently resolved. Read by the resolution tests; the Resolution
    /// variant carries the same fact for the tool layer.
    #[allow(dead_code)]
    pub exact: bool,
}

/// What the reported candidates say about the queried symbol.
///
/// The server's `workspace/symbol` is fuzzy; a result set is *not* claimed
/// exhaustive — "these are the candidates as reported" is the strongest
/// statement available.
#[derive(Debug)]
pub enum Resolution {
    /// Exactly one exact-named candidate: resolvable.
    ExactOne(Box<Candidate>),
    /// More than one exact-named candidate: ambiguous; all are carried for
    /// listing, and none is resolved.
    ExactMany(Vec<Candidate>),
    /// No exact name matched; these are the fuzzy candidates the server
    /// offered as possible matches.
    FuzzyOnly(Vec<Candidate>),
    /// The server reported no candidates at all for this query.
    NotFound,
}

/// [`Resolution`] plus what the filters excluded. Every candidate is first
/// required to lie under the workspace root; an optional scope narrows
/// further on top.
#[derive(Debug)]
pub struct Candidates {
    pub resolution: Resolution,
    /// Candidates whose file lies outside the workspace root; listed in the
    /// result as excluded, not silently dropped.
    pub out_of_root: usize,
    /// Candidates inside the root but outside the requested scope (when one
    /// was given); listed in the result as excluded, not silently dropped.
    pub out_of_scope: usize,
}

/// Partition candidates for `symbol` under the canonical workspace root,
/// optionally narrowed to a canonical scope path inside it.
///
/// Exactness is name equality on the symbol's own name. `containerName` is
/// display metadata only — it is not a language-level path and is not used to
/// validate anything. Each candidate's file is canonicalized before the
/// checks, so a symlink inside the root that points outside is excluded.
/// When the file cannot be canonicalized (missing, broken link), the raw
/// path is used for the check and the candidate cannot be refreshed later —
/// such candidates are never resolvable in practice.
pub fn resolve(
    symbol: &str,
    infos: Vec<SymbolInformation>,
    root: &Path,
    scope: Option<&Path>,
) -> Candidates {
    let mut out_of_root = 0usize;
    let mut out_of_scope = 0usize;
    let mut exact = Vec::new();
    let mut fuzzy = Vec::new();
    for info in infos {
        let file = candidate_path(&info.location.uri);
        // Canonicalize when the file exists so symlinks resolve to their real
        // location before any check; fall back to the raw path otherwise.
        let canonical = file
            .as_ref()
            .and_then(|path| std::fs::canonicalize(path).ok())
            .or(file.clone());
        let in_root = canonical
            .as_ref()
            .is_some_and(|path| path == root || path.starts_with(root));
        if !in_root {
            out_of_root += 1;
            continue;
        }
        if let Some(scope) = scope {
            let in_scope = canonical
                .as_ref()
                .is_some_and(|path| path == scope || path.starts_with(scope));
            if !in_scope {
                out_of_scope += 1;
                continue;
            }
        }
        let candidate = Candidate {
            canonical_path: canonical,
            info,
            exact: false,
        };
        if candidate.info.name == symbol {
            let mut candidate = candidate;
            candidate.exact = true;
            exact.push(candidate);
        } else {
            fuzzy.push(candidate);
        }
    }
    let resolution = match exact.len() {
        0 if fuzzy.is_empty() => Resolution::NotFound,
        0 => Resolution::FuzzyOnly(fuzzy),
        1 => Resolution::ExactOne(Box::new(exact.pop().expect("len 1"))),
        _ => Resolution::ExactMany(exact),
    };
    Candidates {
        resolution,
        out_of_root,
        out_of_scope,
    }
}

/// The filesystem path of a `file:` URI, if it is one.
pub fn candidate_path(uri: &Uri) -> Option<std::path::PathBuf> {
    url::Url::parse(&uri.to_string()).ok()?.to_file_path().ok()
}

/// Where the navigation point came from, stated in results so a mismatch
/// between the symbol's own range and the enclosing range is visible.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavigationProvenance {
    /// `Location.range` — the symbol's own range as the server reported it.
    LocationRange,
    /// `LocationLink.targetSelectionRange` — the symbol's own range inside
    /// the target document.
    LinkSelectionRange,
    /// A `LocationLink` without `targetSelectionRange`; the enclosing
    /// `targetRange` start was used instead. Defensive: the spec requires the
    /// selection range, so reaching this is a server deviation and is stated.
    LinkTargetRange,
}

/// One definition destination.
#[derive(Debug, Clone)]
pub struct DefinitionTarget {
    pub uri: Uri,
    /// The range naming the symbol itself — the navigation point for
    /// positioning and shaping.
    pub selection_range: Range,
    /// The enclosing range the server says the definition spans
    /// (`LocationLink.targetRange`), when reported separately.
    pub full_range: Option<Range>,
    pub provenance: NavigationProvenance,
}

/// What `textDocument/definition` returned, normalized.
#[derive(Debug)]
pub enum DefinitionSet {
    /// `null` or an empty array: the server reports no definition.
    None,
    One(DefinitionTarget),
    Many(Vec<DefinitionTarget>),
}

/// Normalize a `textDocument/definition` result. Accepts every shape the LSP
/// spec allows plus the defensive extras seen in the wild; anything
/// unrecognizable is an error carrying the raw value, never a guess.
pub fn parse_definition_result(value: Value) -> Result<DefinitionSet> {
    let items: Vec<&Value> = match &value {
        Value::Null => return Ok(DefinitionSet::None),
        Value::Array(items) => items.iter().collect(),
        value @ Value::Object(_) => vec![value],
        other => bail!("definition result was neither null, array, nor object: {other}"),
    };
    if items.is_empty() {
        return Ok(DefinitionSet::None);
    }
    let mut targets = Vec::with_capacity(items.len());
    for item in items {
        targets
            .push(parse_target(item).with_context(|| format!("in definition result item {item}"))?);
    }
    match targets.len() {
        1 => Ok(DefinitionSet::One(targets.pop().expect("len 1"))),
        _ => Ok(DefinitionSet::Many(targets)),
    }
}

fn parse_target(item: &Value) -> Result<DefinitionTarget> {
    if item.get("targetUri").is_some() {
        // LocationLink. `lsp_types` types `targetSelectionRange` as required,
        // so the defensive fallback is decided from the raw value, and the
        // malformed-link case is built from raw members rather than the typed
        // parse that would reject exactly what this fallback exists to
        // handle.
        let (selection_range, provenance) = match item.get("targetSelectionRange") {
            Some(value) => (
                serde_json::from_value::<Range>(value.clone())
                    .context("parsing targetSelectionRange")?,
                NavigationProvenance::LinkSelectionRange,
            ),
            None => (
                serde_json::from_value::<Range>(item.get("targetRange").cloned().context(
                    "LocationLink without targetRange and without targetSelectionRange",
                )?)
                .context("parsing targetRange")?,
                NavigationProvenance::LinkTargetRange,
            ),
        };
        // Built from raw members rather than the typed `LocationLink`, whose
        // required `targetSelectionRange` would reject exactly the malformed
        // link this fallback exists to handle.
        let target_uri: Uri =
            serde_json::from_value(item["targetUri"].clone()).context("parsing targetUri")?;
        let target_range: Range = serde_json::from_value(
            item.get("targetRange")
                .cloned()
                .context("LocationLink without targetRange")?,
        )
        .context("parsing targetRange")?;
        Ok(DefinitionTarget {
            uri: target_uri,
            selection_range,
            full_range: Some(target_range),
            provenance,
        })
    } else {
        let location: Location =
            serde_json::from_value(item.clone()).context("parsing Location")?;
        Ok(DefinitionTarget {
            uri: location.uri,
            selection_range: location.range,
            full_range: None,
            provenance: NavigationProvenance::LocationRange,
        })
    }
}

#[cfg(test)]
mod tests {
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
}
