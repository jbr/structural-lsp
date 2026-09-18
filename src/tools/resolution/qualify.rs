//! Qualified names: `Config::new`, `Config.doubled`, `pkg/Greet`. The last
//! segment is the bare name the symbol index is asked for; the earlier
//! segments are a *filter* over the exact candidates by `containerName`,
//! not language-level path resolution. rust-analyzer and pyright report
//! the impl or class type as the container of a method (`new` → `Config`)
//! and nothing for free functions; a qualifier matches when the
//! container's own segments end with it.

use crate::lsp::definition::Candidate;
use anyhow::{Result, bail};

/// A parsed `symbol` argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Qualified {
    /// The bare name, sent to `workspace/symbol` and matched exactly.
    pub name: String,
    /// The container segments that must end the candidate's container.
    pub qualifiers: Vec<String>,
}

impl Qualified {
    /// The name as the user spelled it, for messages.
    pub fn display(&self) -> String {
        let mut segments = self.qualifiers.clone();
        segments.push(self.name.clone());
        segments.join("::")
    }
}

/// The separators a qualified name or a container may use.
fn segments(text: &str) -> Vec<&str> {
    text.split("::")
        .flat_map(|part| part.split(['.', '/']))
        .map(trim_to_identifier)
        .collect()
}

/// A segment with anything that cannot be part of an identifier trimmed
/// from its ends.
///
/// gopls spells a method `(Config).Doubled` in an outline and
/// `Config.Doubled` in the index; the parentheses are punctuation in the
/// server's own spelling, not part of the type's name. Trimming the ends
/// only — never the middle — keeps `Config` and leaves anything with
/// interior punctuation alone, so this cannot quietly turn one name into
/// another.
fn trim_to_identifier(segment: &str) -> &str {
    segment.trim_matches(|c: char| !(c.is_alphanumeric() || c == '_'))
}

/// The segments of a name a **server advertised**, when its last segment is
/// `name`: the earlier ones are that server's own spelling of the
/// container.
///
/// `SymbolInformation.name` is not specified to be a bare identifier, and
/// gopls puts the receiver in it. The last segment must be the name being
/// resolved and the split must have produced more than one segment, so a
/// name with no separator in it is never reinterpreted.
pub fn advertised_container(advertised: &str, name: &str) -> Option<String> {
    let parts = segments(advertised);
    let (last, earlier) = parts.split_last()?;
    if *last != name || earlier.is_empty() {
        return None;
    }
    Some(earlier.join("::"))
}

/// Whether a name a server advertised resolves the name being asked for:
/// equal, or equal in its last segment.
pub fn advertises(advertised: &str, name: &str) -> bool {
    advertised == name || advertised_container(advertised, name).is_some()
}

/// Parse `symbol`: non-empty, no empty segments.
pub fn parse(symbol: &str) -> Result<Qualified> {
    let trimmed = symbol.trim();
    if trimmed.is_empty() {
        bail!("`symbol` must be a non-empty name (e.g. `Config` or `Config::new`)");
    }
    let parts = segments(trimmed);
    if parts.iter().any(|part| part.trim().is_empty()) {
        bail!("`symbol` `{trimmed}` has an empty segment");
    }
    let (name, qualifiers) = parts.split_last().expect("non-empty");
    Ok(Qualified {
        name: name.trim().to_string(),
        qualifiers: qualifiers
            .iter()
            .map(|part| part.trim().to_string())
            .collect(),
    })
}

/// Whether `container` (as the server reported it, `None` for a free item)
/// ends with `qualifiers`.
pub fn container_matches(container: Option<&str>, qualifiers: &[String]) -> bool {
    if qualifiers.is_empty() {
        return true;
    }
    let Some(container) = container else {
        return false;
    };
    let parts = segments(container);
    parts.len() >= qualifiers.len()
        && parts[parts.len() - qualifiers.len()..]
            .iter()
            .zip(qualifiers)
            .all(|(part, qualifier)| part == qualifier)
}

/// Split exact candidates into those whose container matches the
/// qualifiers and those whose does not.
///
/// A container has more than one possible source and servers disagree about
/// which they use (`docs/resolution.md`): the `containerName` field, or the
/// server's own spelling of the symbol's name. Either confirming the
/// qualifier is enough — they are alternative places to say the same thing,
/// not claims that have to agree — and a candidate no source confirms is
/// listed rather than silently dropped, as before.
pub fn filter(
    candidates: Vec<Candidate>,
    qualified: &Qualified,
) -> (Vec<Candidate>, Vec<Candidate>) {
    candidates.into_iter().partition(|candidate| {
        let sources = [
            candidate.info.container_name.clone(),
            advertised_container(&candidate.info.name, &qualified.name),
            file_container(candidate),
        ];
        sources
            .iter()
            .any(|source| container_matches(source.as_deref(), &qualified.qualifiers))
    })
}

/// The container a symbol's **file** gives it, for languages whose module
/// is a file: `helper.choose` in `src/helper.gleam`, `Config.doubled` in
/// `Config.qml`.
///
/// gleam and qmlls nominate a symbol from a file's outline and report no
/// container from any other source, because there is no enclosing node to
/// report — the module is the file, and the file name is not in the reply.
/// The stem is read from the candidate's own path rather than inferred, so
/// this is a fact about where the symbol is, not a guess about what it is
/// called.
///
/// It is offered as one source among several and never overrides another.
/// A language whose files are not modules gains a qualifier that happens to
/// work; a query where a file and a type share a name matches both and is
/// listed as ambiguous, which is the contract everywhere else
/// (`docs/correctness.md`).
fn file_container(candidate: &Candidate) -> Option<String> {
    candidate
        .canonical_path
        .as_ref()?
        .file_stem()?
        .to_str()
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_split_on_any_qualifier_separator() {
        let parsed = parse("Config::new").unwrap();
        assert_eq!(parsed.name, "new");
        assert_eq!(parsed.qualifiers, ["Config"]);
        assert_eq!(parse("Config.doubled").unwrap().qualifiers, ["Config"]);
        assert_eq!(parse("a::b.c/d").unwrap().qualifiers, ["a", "b", "c"]);
        assert_eq!(
            parse("  choose ").unwrap(),
            Qualified {
                name: "choose".into(),
                qualifiers: vec![]
            }
        );
        assert_eq!(parse("Config::new").unwrap().display(), "Config::new");
        assert!(parse("").is_err());
        assert!(parse("Config::").is_err());
        assert!(parse("::new").is_err());
    }

    /// gopls spells the receiver into the name: `Config.Doubled` from the
    /// index, `(Config).Doubled` from an outline. Both name `Doubled` and
    /// both say its container is `Config`.
    #[test]
    fn a_server_may_spell_the_container_into_the_name() {
        for advertised in ["Config.Doubled", "(Config).Doubled", "Config::Doubled"] {
            assert!(advertises(advertised, "Doubled"), "{advertised}");
            assert_eq!(
                advertised_container(advertised, "Doubled").as_deref(),
                Some("Config"),
                "{advertised}"
            );
        }
        // Several segments are kept in order, so a module path qualifies
        // the same way a type does.
        assert_eq!(
            advertised_container("pkg/inner.Doubled", "Doubled").as_deref(),
            Some("pkg::inner")
        );
    }

    /// The last segment must be the name asked for, and a name with no
    /// separator is never reinterpreted.
    #[test]
    fn only_the_last_segment_resolves_and_a_plain_name_is_untouched() {
        assert!(advertises("Doubled", "Doubled"));
        assert_eq!(advertised_container("Doubled", "Doubled"), None);
        // The name is the container's, not the symbol's.
        assert!(!advertises("Config.Doubled", "Config"));
        // A different trailing segment is a different symbol.
        assert!(!advertises("Config.Other", "Doubled"));
        // Interior punctuation is left alone: only the ends are trimmed.
        assert_eq!(
            advertised_container("a-b.Doubled", "Doubled").as_deref(),
            Some("a-b")
        );
    }

    /// Either source confirming the qualifier is enough, because they are
    /// alternative places to say the same thing: ty reports no container at
    /// all, gopls reports the package in `containerName` and the type in
    /// the name.
    #[test]
    fn either_container_source_satisfies_the_qualifier() {
        // From JSON, as the rest of the crate builds these: the struct
        // literal would name a deprecated field.
        let candidate = |name: &str, container: Option<&str>| Candidate {
            server: "s".into(),
            info: serde_json::from_value(serde_json::json!({
                "name": name,
                "kind": 6,
                "location": {
                    "uri": "file:///x",
                    "range": {
                        "start": { "line": 0, "character": 0 },
                        "end": { "line": 0, "character": 0 }
                    }
                },
                "containerName": container,
            }))
            .expect("a SymbolInformation"),
            canonical_path: None,
            exact: true,
        };
        let asked = parse("Config::Doubled").unwrap();

        // The field says it (pyright's shape).
        let (kept, _) = filter(vec![candidate("Doubled", Some("Config"))], &asked);
        assert_eq!(kept.len(), 1, "containerName confirms it");

        // The name says it and the field names the package (gopls's shape).
        let (kept, _) = filter(
            vec![candidate("Config.Doubled", Some("example.com/scratch"))],
            &asked,
        );
        assert_eq!(kept.len(), 1, "the advertised name confirms it");

        // Neither says it: listed, not silently dropped.
        let (kept, unqualified) = filter(vec![candidate("Doubled", Some("Other"))], &asked);
        assert!(kept.is_empty());
        assert_eq!(unqualified.len(), 1);
    }

    /// gleam and qmlls report no container at all: the module is the file,
    /// and the file name is not in the reply.
    #[test]
    fn a_module_that_is_a_file_qualifies_from_the_path() {
        let candidate = |name: &str, path: &str| Candidate {
            server: "s".into(),
            info: serde_json::from_value(serde_json::json!({
                "name": name,
                "kind": 12,
                "location": {
                    "uri": "file:///x",
                    "range": {
                        "start": { "line": 0, "character": 0 },
                        "end": { "line": 0, "character": 0 }
                    }
                },
            }))
            .expect("a SymbolInformation"),
            canonical_path: Some(std::path::PathBuf::from(path)),
            exact: true,
        };

        let (kept, _) = filter(
            vec![candidate("choose", "/ws/src/helper.gleam")],
            &parse("helper.choose").unwrap(),
        );
        assert_eq!(kept.len(), 1, "the module is the file");

        let (kept, _) = filter(
            vec![candidate("doubled", "/ws/Config.qml")],
            &parse("Config.doubled").unwrap(),
        );
        assert_eq!(kept.len(), 1, "the component is the file");

        // Another file's symbol is not in this module.
        let (kept, unqualified) = filter(
            vec![candidate("choose", "/ws/src/other.gleam")],
            &parse("helper.choose").unwrap(),
        );
        assert!(kept.is_empty());
        assert_eq!(unqualified.len(), 1);
    }

    #[test]
    fn a_qualifier_is_a_suffix_of_the_container() {
        let q = |parts: &[&str]| parts.iter().map(|p| p.to_string()).collect::<Vec<_>>();
        assert!(container_matches(Some("Config"), &q(&["Config"])));
        assert!(container_matches(
            Some("crate::config::Config"),
            &q(&["Config"])
        ));
        assert!(container_matches(
            Some("config::Config"),
            &q(&["config", "Config"])
        ));
        assert!(!container_matches(Some("Other"), &q(&["Config"])));
        assert!(!container_matches(None, &q(&["Config"])));
        assert!(container_matches(None, &q(&[])));
        assert!(!container_matches(Some("Config"), &q(&["mod", "Config"])));
    }
}
