//! Symbol resolution and definition-result normalization, as pure functions.
//!
//! No I/O and no language server: [`resolve`] partitions `workspace/symbol`
//! candidates into exact/fuzzy/ambiguous, and [`parse_definition_result`]
//! defensively normalizes what `textDocument/definition` returns (`Location`,
//! `Location[]`, `LocationLink[]`, `LocationLink`, `null`, `[]` — every shape
//! the spec allows). The MCP tool layer composes these with the session.

use crate::lsp::positions::{Encoding, offset_to_position, position_to_offset};
use anyhow::{Context, Result, bail};
use lsp_types::{Location, Position, Range, SymbolInformation, Uri};
use serde_json::Value;
#[cfg(test)]
use serde_json::json;
use std::path::Path;

/// One `workspace/symbol` candidate as the server reported it, with the
/// canonicalized file path used for scope filtering and refreshing.
#[derive(Clone, Debug)]
pub struct Candidate {
    /// The adapter name of the server that reported it; the same server
    /// answers the definition query.
    pub server: String,
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
    /// Candidates in a file that is no longer on disk: the index has not
    /// caught up with a deletion. Listed as excluded, not silently dropped,
    /// and never navigated (docs/correctness.md).
    pub missing: usize,
}

/// One server's candidates, partitioned but not yet judged: several servers'
/// partitions are merged before the resolution is decided.
#[derive(Debug, Default)]
pub struct Partition {
    exact: Vec<Candidate>,
    fuzzy: Vec<Candidate>,
    pub out_of_root: usize,
    pub out_of_scope: usize,
    pub missing: usize,
}

impl Partition {
    pub fn absorb(&mut self, other: Partition) {
        self.exact.extend(other.exact);
        self.fuzzy.extend(other.fuzzy);
        self.out_of_root += other.out_of_root;
        self.out_of_scope += other.out_of_scope;
        self.missing += other.missing;
    }

    /// Promote fuzzy candidates whose advertised name **decorates**
    /// `symbol`, when nothing matched it by name.
    ///
    /// A server's `name` is a display string, not a declared identifier:
    /// ccls advertises `int choose(bool flag)` in its index and elp
    /// advertises `choose/1` in an outline, both for a definition the file
    /// spells `choose`. When the decoration mentions the queried name
    /// exactly once as a whole token, the symbol is nominated under the
    /// queried name — and the range still has to hold it in the text just
    /// read, which is checked before anything navigates
    /// (docs/resolution.md).
    ///
    /// Only as a fallback: a server that advertises the bare name resolves
    /// exactly as it did, and a decorated candidate is never an additional
    /// exact match beside one. `sole_token` is the whole rule — no
    /// language-specific parsing of what a decoration may be.
    pub fn promote_decorated(&mut self, symbol: &str) {
        if !self.exact.is_empty() {
            return;
        }
        let (decorated, rest): (Vec<Candidate>, Vec<Candidate>) = std::mem::take(&mut self.fuzzy)
            .into_iter()
            .partition(|candidate| decorates(&candidate.info.name, symbol));
        self.exact = decorated
            .into_iter()
            .map(|mut candidate| {
                // The identity is the identifier the range names; the
                // decoration was the server's spelling of it.
                candidate.info.name = symbol.to_string();
                candidate.exact = true;
                candidate
            })
            .collect();
        self.fuzzy = rest;
    }

    /// Set aside candidates whose file is no longer on disk, counting them.
    ///
    /// Such a candidate is the index answering from before a deletion it
    /// has been told of and not yet applied — rust-analyzer applies a
    /// deleted file on its loader thread, after the query that follows the
    /// notice. It cannot be navigated, and it is not a definition that
    /// exists (docs/correctness.md).
    pub fn drop_missing(&mut self) {
        let before = self.exact.len() + self.fuzzy.len();
        let on_disk = |candidate: &Candidate| {
            candidate
                .canonical_path
                .as_ref()
                .is_some_and(|path| path.is_file())
        };
        self.exact.retain(on_disk);
        self.fuzzy.retain(on_disk);
        self.missing += before - (self.exact.len() + self.fuzzy.len());
    }

    pub fn into_candidates(self) -> Candidates {
        let Partition {
            mut exact,
            fuzzy,
            out_of_root,
            out_of_scope,
            missing,
        } = self;
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
            missing,
        }
    }
}

/// Partition one server's candidates for `symbol` under the canonical
/// workspace root, optionally narrowed to a canonical scope path inside it.
///
/// Exactness is name equality on the symbol's own name. `containerName` is
/// display metadata only — it is not a language-level path and is not used to
/// validate anything. Each candidate's file is canonicalized before the
/// checks, so a symlink inside the root that points outside is excluded.
/// When the file cannot be canonicalized (missing, broken link), the raw
/// path is used for the check and the candidate cannot be synced later —
/// such candidates are never resolvable in practice.
pub fn partition(
    symbol: &str,
    server: &str,
    infos: Vec<SymbolInformation>,
    root: &Path,
    scope: Option<&Path>,
) -> Partition {
    let mut out = Partition::default();
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
            out.out_of_root += 1;
            continue;
        }
        if let Some(scope) = scope {
            let in_scope = canonical
                .as_ref()
                .is_some_and(|path| path == scope || path.starts_with(scope));
            if !in_scope {
                out.out_of_scope += 1;
                continue;
            }
        }

        // Exact by the name's **last segment**, not by the whole string. A
        // server may spell the container into the name it advertises —
        // gopls says `Config.Doubled` in the index and `(Config).Doubled`
        // in an outline — and `SymbolInformation.name` is not specified to
        // be a bare identifier. A name with no separator in it is
        // unaffected, and the range is verified against the file either way
        // (docs/resolution.md).
        let exact = crate::tools::resolution::qualify::advertises(&info.name, symbol);
        let candidate = Candidate {
            server: server.to_string(),
            canonical_path: canonical,
            info,
            exact,
        };
        if exact {
            out.exact.push(candidate);
        } else {
            out.fuzzy.push(candidate);
        }
    }
    out
}

/// [`partition`] and judge in one step, for a single server.
pub fn resolve(
    symbol: &str,
    server: &str,
    infos: Vec<SymbolInformation>,
    root: &Path,
    scope: Option<&Path>,
) -> Candidates {
    let mut partitioned = partition(symbol, server, infos, root, scope);
    partitioned.promote_decorated(symbol);
    partitioned.into_candidates()
}

/// One *definition target* that exact-named candidates lead to: the unit of
/// identity. Several `workspace/symbol` hits that navigate to the same place
/// are one thing; hits that navigate to different places are different
/// things, whatever their names.
#[derive(Debug)]
pub struct Identity {
    pub server: String,
    /// Where `textDocument/definition` went from the origin, or `None` when
    /// the server reported no definition for it.
    pub target: Option<DefinitionTarget>,
    /// What the identity was reached from: the first exact candidate, or
    /// the focus of an anchor.
    pub origin: Origin,
    /// Every exact candidate that led here, in discovery order; empty for
    /// an anchored identity.
    pub candidates: Vec<Candidate>,
}

/// Where a resolution started, for headers and listings when the outline
/// cannot name the target itself.
#[derive(Debug, Clone)]
pub struct Origin {
    pub name: String,
    /// The symbol index's kind for a candidate; `None` for an anchor, whose
    /// focus has no kind until the outline confirms one.
    pub kind: Option<lsp_types::SymbolKind>,
    /// The candidate's own location, or the focus's.
    pub location: Location,
}

impl Origin {
    pub fn of(candidate: &Candidate) -> Self {
        Self {
            name: candidate.info.name.clone(),
            kind: Some(candidate.info.kind),
            location: candidate.info.location.clone(),
        }
    }
}

impl Identity {
    /// The origin's kind, for listings.
    pub fn kind(&self) -> Option<lsp_types::SymbolKind> {
        self.origin.kind
    }

    pub fn name(&self) -> &str {
        &self.origin.name
    }

    /// An identity reached from an anchor's focus rather than a candidate.
    pub fn anchored(server: &str, origin: Origin, target: Option<DefinitionTarget>) -> Self {
        Self {
            server: server.to_string(),
            target,
            origin,
            candidates: Vec::new(),
        }
    }
}

/// Group each candidate's definition result by target. A candidate whose
/// server reported several targets contributes to each; one with no target
/// is its own identity, keyed by its own location, so it is listed rather
/// than dropped. Order is first appearance.
pub fn group_by_target(found: Vec<(Candidate, DefinitionSet)>) -> Vec<Identity> {
    let mut identities: Vec<Identity> = Vec::new();
    let mut place = |candidate: Candidate, target: Option<DefinitionTarget>| {
        let key = |identity: &Identity| -> bool {
            identity.server == candidate.server
                && match (&identity.target, &target) {
                    (Some(a), Some(b)) => a.uri == b.uri && a.selection_range == b.selection_range,
                    (None, None) => identity.origin.location == candidate.info.location,
                    _ => false,
                }
        };
        match identities.iter_mut().find(|identity| key(identity)) {
            Some(identity) => identity.candidates.push(candidate),
            None => identities.push(Identity {
                server: candidate.server.clone(),
                target,
                origin: Origin::of(&candidate),
                candidates: vec![candidate],
            }),
        }
    };
    for (candidate, set) in found {
        match set {
            DefinitionSet::None => place(candidate, None),
            DefinitionSet::One(target) => place(candidate, Some(target)),
            DefinitionSet::Many(targets) => {
                for target in targets {
                    place(candidate.clone(), Some(target));
                }
            }
        }
    }
    identities
}

/// Where a server's reported range puts the symbol's name.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum NameLocation {
    /// The range names no token: empty, or the whole file — a module whose
    /// definition *is* its file. There is nothing to check and nothing to
    /// navigate from but the range itself.
    Unnamed,
    /// The name is at this position. Equal to the range's start when the
    /// range is exactly the name, and inside it when the range is wider.
    At(Position),
}

impl NameLocation {
    /// The position to send a follow-up request at, given the range this
    /// came from.
    pub fn position(&self, range: Range) -> Position {
        match self {
            NameLocation::Unnamed => range.start,
            NameLocation::At(position) => *position,
        }
    }
}

/// Find `name` within `range` in `text` — the text the server was just
/// given for that file — and say where it is.
///
/// The check is that the server's index agrees with the text just read: a
/// range holding no such name means the answer came from a stale index, and
/// the caller reports that rather than navigating from it
/// (docs/correctness.md).
///
/// **A wider range is not staleness.** `SymbolInformation.location.range` is
/// specified as the range enclosing the symbol, so a server may report a
/// whole declaration — `def choose(flag: bool) -> int:` — where another
/// reports just `choose`. `DocumentSymbol` carries `selectionRange` for the
/// name and `SymbolInformation` has nowhere to put it, so the tight range
/// is a convention, not a requirement. Both shapes resolve here, and the
/// position returned is the name's either way, because that is what a
/// `definition` or `references` request has to be sent at.
///
/// Two occurrences of the name inside one range is ambiguous, and nothing
/// ambiguous is silently picked (`docs/correctness.md`).
pub fn locate_name_at(
    text: &str,
    range: Range,
    name: &str,
    encoding: Encoding,
) -> Result<NameLocation> {
    let start = position_to_offset(text, range.start, encoding)
        .context("the reported range starts outside the file as read from disk")?;
    let end = position_to_offset(text, range.end, encoding)
        .context("the reported range ends outside the file as read from disk")?;
    if start == end || (start == 0 && end == text.len()) {
        return Ok(NameLocation::Unnamed);
    }
    if end < start {
        bail!("the reported range is inverted");
    }
    let found = &text[start..end];
    if found == name {
        return Ok(NameLocation::At(range.start));
    }
    if name.is_empty() {
        bail!("the reported name is empty");
    }
    let mut occurrences = found
        .match_indices(name)
        .filter(|(at, _)| is_whole_token(found, *at, name.len()));
    let (first, _) = occurrences.next().ok_or_else(|| {
        anyhow::anyhow!(
            "expected `{name}` at {}:{}, found `{}`",
            range.start.line + 1,
            range.start.character + 1,
            elided(found)
        )
    })?;
    if occurrences.next().is_some() {
        bail!(
            "`{name}` occurs more than once in the range reported at {}:{}, so which one the \
             server meant is not decidable",
            range.start.line + 1,
            range.start.character + 1
        );
    }
    Ok(NameLocation::At(offset_to_position(
        text,
        start + first,
        encoding,
    )?))
}

/// The single whole-token occurrence of `name` in `text`, or `None` for
/// zero or several.
///
/// The same rule [`locate_name_at`] applies to a range wider than the name
/// (docs/correctness.md), exposed for callers that hold text rather than a
/// range: a container's selection range is the other place a server may
/// report more than the name.
pub fn sole_token(text: &str, name: &str) -> Option<usize> {
    if name.is_empty() {
        return None;
    }
    let mut occurrences = text
        .match_indices(name)
        .filter(|(at, _)| is_whole_token(text, *at, name.len()))
        .map(|(at, _)| at);
    let first = occurrences.next()?;
    occurrences.next().is_none().then_some(first)
}

/// Whether the `len` bytes at `at` are bounded by non-identifier characters
/// — so `choose` in `def choose(` matches and the one in `chooser` does not.
fn is_whole_token(haystack: &str, at: usize, len: usize) -> bool {
    let boundary =
        |character: Option<char>| !character.is_some_and(|c| c.is_alphanumeric() || c == '_');
    boundary(haystack[..at].chars().next_back()) && boundary(haystack[at + len..].chars().next())
}

/// A whole declaration can be long, and the text lands in an error message.
pub fn elided(found: &str) -> String {
    let line = found.lines().next().unwrap_or(found);
    if line.chars().count() > 60 {
        format!("{}…", line.chars().take(59).collect::<String>())
    } else {
        line.to_string()
    }
}

/// Check that `range` names `name` in `text`, discarding where.
pub fn verify_name_at(text: &str, range: Range, name: &str, encoding: Encoding) -> Result<()> {
    locate_name_at(text, range, name, encoding).map(|_| ())
}

/// Whether `advertised` is a decoration of `name`: a different string that
/// mentions it exactly once as a whole token.
///
/// `choose/1` and `int choose(bool flag)` decorate `choose`; `chooser` and
/// `choose_all` do not (no token boundary), and neither does `choose` itself
/// (that is a name, not a decoration of one).
pub fn decorates(advertised: &str, name: &str) -> bool {
    advertised != name && sole_token(advertised, name).is_some()
}

/// Whether `range` is the whole of `text`: a definition target that is a
/// file rather than a token in one.
pub fn is_whole_file(text: &str, range: Range, encoding: Encoding) -> bool {
    range.start == lsp_types::Position::new(0, 0)
        && position_to_offset(text, range.end, encoding).is_ok_and(|end| end == text.len())
        && !text.is_empty()
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
mod tests;
