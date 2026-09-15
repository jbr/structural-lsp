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
        .collect()
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
pub fn filter(
    candidates: Vec<Candidate>,
    qualifiers: &[String],
) -> (Vec<Candidate>, Vec<Candidate>) {
    candidates.into_iter().partition(|candidate| {
        container_matches(candidate.info.container_name.as_deref(), qualifiers)
    })
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
