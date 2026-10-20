//! Qualified names: `Config::new`, `Config.doubled`, `pkg/Greet`,
//! `listing::render`. The last segment is the bare name the symbol index is
//! asked for; the earlier segments are a *filter* over the exact candidates,
//! not language-level path resolution. A qualifier matches when it ends one
//! of the places a candidate is said to be: the container its server
//! reported, the items its file's outline encloses it in, its file's path,
//! or the directory its file is in. Only the candidates it names most
//! closely are kept ([`Closeness`]), so a qualifier that names a file's
//! top-level function is not shared with a same-named method or test helper
//! inside that file (`docs/resolution.md`).

use crate::lsp::definition::Candidate;
use crate::workspace::Workspace;
use anyhow::{Result, bail};
use std::path::Path;

mod nesting;
mod placed;

pub use placed::{Closeness, Placed};

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

/// Split exact candidates into those the qualifier selects and those it
/// does not.
///
/// rust-analyzer reports no container for a free function, even one inside
/// `mod tests`, so for such a candidate the file's outline is asked what
/// encloses it (the `nesting` module) whenever that could change the
/// answer: the qualifier already ends its file or directory and the outline
/// says whether it sits at the top of that file or inside something there,
/// or the file's text holds the qualifier's last segment and the outline
/// could place it under that name. A candidate whose server reported a
/// container is asked only when that container ends the way the qualifier
/// does and the qualifier is longer ([`Placed::asks_outline`]). A `path`
/// naming a file nominated from that outline already (`outlined`), so its
/// candidates carry their nesting.
pub fn filter(
    state: &mut Workspace,
    root: &Path,
    candidates: Vec<Candidate>,
    qualified: &Qualified,
    outlined: bool,
) -> Result<(Vec<Candidate>, Vec<Candidate>)> {
    if qualified.qualifiers.is_empty() {
        return Ok((candidates, Vec::new()));
    }
    let mut placed = Vec::with_capacity(candidates.len());
    for candidate in candidates {
        let mut place = Placed::new(root, candidate);
        if !outlined
            && place.asks_outline(qualified, || {
                nesting::mentioned(&place.candidate, qualified)
            })
        {
            let found = nesting::nesting_of(state, root, &place.candidate, &qualified.name)?;
            place.set_nesting(found);
        }
        placed.push(place);
    }
    Ok(partition(placed, qualified))
}

/// Keep the candidates the qualifier names most closely ([`Closeness`]); the
/// rest are the unqualified listing.
///
/// A closer match settles the query rather than sharing a listing with a
/// looser one: `listing::render` means the function at the top of
/// `listing.rs`, not a same-named method or test helper inside it, and
/// `views::run` means the one in `views.rs`, not those in `views/*.rs` —
/// measured on this repository, keeping every match together turned
/// `views::run` from one answer into four (`docs/resolution.md`). Several
/// candidates equally close are all kept and listed as ambiguous
/// downstream; nothing here picks among them.
pub fn partition(placed: Vec<Placed>, qualified: &Qualified) -> (Vec<Candidate>, Vec<Candidate>) {
    let closest = placed
        .iter()
        .filter_map(|place| place.closeness(qualified))
        .min();
    let (kept, rest): (Vec<Placed>, Vec<Placed>) = placed
        .into_iter()
        .partition(|place| closest.is_some() && place.closeness(qualified) == closest);
    (
        kept.into_iter().map(|place| place.candidate).collect(),
        rest.into_iter().map(|place| place.candidate).collect(),
    )
}

#[cfg(test)]
mod tests;
