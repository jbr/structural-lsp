//! Nominating from a named file's own outline.
//!
//! A `path` that names a file is not a filter over the servers' symbol
//! index. `workspace/symbol` answers with a bounded, fuzzy-ordered pool
//! (rust-analyzer truncates it at 128 results — measured: a 200-definition
//! file comes back with its first 128 in file order and the rest missing),
//! so a common name's later definitions never enter the pool and a
//! qualifier has nothing to narrow. The file's own
//! `textDocument/documentSymbol` tree is the server's statement of what that
//! one file declares: read after the file is synced from disk, nested, and
//! not sampled from a bounded cross-workspace response — though it is not a
//! proven-exhaustive declaration index either (a macro-generated item may
//! not be listed).
//!
//! Nomination identity is the server's **advertised name**: a node
//! nominates when `DocumentSymbol.name` is exactly the name asked for, and
//! its own selection range is the navigation point handed to the pipeline.
//! That range is *not* where the name comes from. The advertised name is
//! checked against the text at the range here, on the text already in hand,
//! before the candidate can reach a filter or a listing — and the pipeline
//! checks it again before navigating (`definition_of`). Reading a name out
//! of the file instead would make both checks vacuous, and they exist
//! precisely because a server's outline and the file on disk can disagree.
//!
//! An `impl Config` node is therefore not a candidate: its advertised name
//! is `impl Config`, which no query asks for. It is still a **container**,
//! because rust-analyzer nests a method under it and reports the container
//! `Config` for that method in its symbol index. What an enclosing node
//! contributes is its own adapter's rule (`Adapter::outline_container`): for
//! an ordinary symbol the range must name the symbol itself, and
//! rust-analyzer's impl node contributes the type it is written for. A
//! shape that rule does not recognize is an error, never the range's text
//! taken to be a container, because a container feeds the qualifier filter
//! and a wrong one selects a different definition.
//!
//! The flat `SymbolInformation[]` shape carries no range that names each
//! symbol, only `location.range` for the whole item. Its **declaration
//! line** is searched for the advertised name instead — every language here
//! writes a declaration's name on its first line — and a line that does not
//! name the symbol, or names it twice, nominates nothing. The whole item is
//! deliberately not searched: a name recurs inside its own body, and an
//! ambiguous nomination is not one. pylsp 1.15.0 serves this shape.

use crate::lsp::adapter::Adapter;
use crate::lsp::definition::{Candidate, NameLocation, decorates, locate_name_at, verify_name_at};
use crate::lsp::outline::{self, Node};
use crate::lsp::positions::{Encoding, position_to_offset};
use crate::lsp::server::LspServer;
use crate::workspace::Workspace;
use anyhow::{Context, Result, bail};
use lsp_types::{Position, Range, SymbolInformation, Uri};
use serde_json::Value;
use std::path::Path;

/// The candidates the selected servers' outlines of `file` give for
/// `symbol`, in outline order, after the pipeline's sync-and-wait. Empty
/// when there is no outline to nominate from (no `documentSymbol`
/// capability, a null result) or it advertises nothing with
/// exactly that name — the caller falls back to the servers' symbol index,
/// so a file scope is never weaker than a bare name. An outline range that
/// does not match the name it is claimed for is an error, not an empty
/// answer.
pub fn nominate(
    state: &mut Workspace,
    selected: &[usize],
    file: &Path,
    symbol: &str,
) -> Result<Vec<Candidate>> {
    // The wait is unbounded except by the inactivity backstop.
    for &index in selected {
        let server = state.server_mut(index)?;
        server.sync_owned()?;
        server.wait_ready()?;
    }
    let mut candidates = Vec::new();
    for &index in selected {
        let server = state.server_mut(index)?;
        candidates.extend(from_server(server, file, symbol)?);
    }
    Ok(candidates)
}

/// One server's outline of `file`, turned into candidates for `symbol`. The
/// file is synced first, so the outline is of the text on disk now.
fn from_server(server: &mut LspServer, file: &Path, symbol: &str) -> Result<Vec<Candidate>> {
    outline_of(server, file, symbol).map(|(_, candidates)| candidates)
}

/// [`from_server`], with the text the outline was read against: a qualifier
/// that asks an index candidate's outline what encloses it checks the
/// candidate's own range against the same text (`qualify`).
pub(super) fn outline_of(
    server: &mut LspServer,
    file: &Path,
    symbol: &str,
) -> Result<(String, Vec<Candidate>)> {
    let synced = server.sync(file)?;
    // The sync precedes this request on the wire, so the outline is the
    // server's answer for the text just read; wait in case it started work.
    server.wait_ready()?;
    let Some(outline) = server.document_symbol(&synced.uri)? else {
        return Ok((synced.text, Vec::new()));
    };
    let uri: Uri =
        serde_json::from_value(Value::String(synced.uri.clone())).with_context(|| {
            format!(
                "{}'s document URI for {} is not a URI",
                server.name(),
                file.display()
            )
        })?;
    let named = NamedFile {
        text: &synced.text,
        encoding: server.encoding(),
        symbol,
        server: server.name(),
        adapter: server.adapter(),
        uri,
        path: file,
        decorated: false,
    };
    let found = candidates(&outline, &named)?;
    // An outline that names the symbol is an answer even though the index
    // never spoke: a server that only serves documents still serves.
    if !found.is_empty() {
        server.mark_answered();
    }
    Ok((synced.text, found))
}

/// The pure part: an outline result and the file it was read from, to the
/// candidates it advertises for that file's queried name.
///
/// Two passes, the second only when the first found nothing: a node whose
/// advertised name *is* the queried name, then a node whose advertised name
/// **decorates** it — `choose/1` for `choose`, which is how both Erlang
/// servers name a function (`docs/resolution.md`). The fallback order is
/// what keeps a server that advertises the bare name resolving exactly as
/// it did.
fn candidates(outline: &Value, file: &NamedFile<'_>) -> Result<Vec<Candidate>> {
    let nodes = outline::tree(outline)
        .with_context(|| format!("{} returned an unusable documentSymbol result", file.server))?;
    let mut found = Vec::new();
    collect(&nodes, &mut Vec::new(), file, &mut found)?;
    if found.is_empty() && !file.decorated {
        let decorated = NamedFile {
            decorated: true,
            uri: file.uri.clone(),
            ..*file
        };
        collect(&nodes, &mut Vec::new(), &decorated, &mut found)?;
    }
    Ok(found)
}

/// The file a nomination is reading: the text just read from disk, the
/// server's encoding for it, the query's bare name, and the adapter whose
/// outline rules this server's symbols follow.
struct NamedFile<'a> {
    text: &'a str,
    encoding: Encoding,
    symbol: &'a str,
    server: &'a str,
    adapter: &'a dyn Adapter,
    uri: Uri,
    path: &'a Path,
    /// Which pass this is: nodes advertising exactly the queried name, or —
    /// when that pass found nothing — nodes whose advertised name
    /// **decorates** it (`decorates`, `crate::lsp::definition`).
    decorated: bool,
}

impl NamedFile<'_> {
    /// Whether `node` nominates in this pass: it advertises the queried
    /// name, or — in the decorated pass — its advertised name decorates it.
    fn nominates(&self, node: &Node) -> bool {
        if self.decorated {
            decorates(&node.extent.name, self.symbol)
        } else {
            node.extent.name == self.symbol
        }
    }

    /// The identifier a nominated node is nominated under, and the name
    /// every range check is made against: the queried name in the decorated
    /// pass, where the advertised name is the server's spelling of it and
    /// the range is what proves it.
    fn identifier<'n>(&'n self, node: &'n Node) -> &'n str {
        if self.decorated {
            self.symbol
        } else {
            node.extent.name.as_str()
        }
    }
}

/// Every symbol `symbols` advertises with exactly the asked-for name, with
/// `ancestors` — the enclosing symbols' advertised names and selection
/// ranges — to name their containers.
///
/// The advertised name is the identity; the selection range is only the
/// navigation point, and the pipeline checks the name against the text there
/// before navigating. An `impl Config` node advertises `impl Config`, so it
/// is never a candidate; it is still an ancestor, whose range names the
/// container of the methods under it.
fn collect<'a>(
    symbols: &'a [Node],
    ancestors: &mut Vec<(&'a str, Range)>,
    file: &NamedFile<'_>,
    out: &mut Vec<Candidate>,
) -> Result<()> {
    for node in symbols {
        // The flat `SymbolInformation[]` shape carries no range that names
        // the symbol, only the whole item's. Its declaration line does name
        // it, so that line is searched instead: pylsp 1.15.0 serves this
        // shape and nothing in a file scope could be nominated from it
        // before (docs/resolution.md). A node whose declaration line does
        // not name it, or names it twice, yields no range and is skipped —
        // its children are still collected.
        let Some(range) = node
            .extent
            .selection_range
            .or_else(|| declaration_range(file, node))
        else {
            collect(&node.children, ancestors, file, out)?;
            continue;
        };
        if file.nominates(node) {
            let identifier = file.identifier(node);
            // Checked here, on the text already in hand, before the candidate
            // can reach a filter or a listing: a listing built on a range the
            // server no longer matches is exactly what the contract forbids.
            let named = verify_name_at(file.text, range, identifier, file.encoding);
            if file.decorated {
                // The node never advertised this name — the decoration did.
                // A range that does not hold the identifier says only that
                // this is not the symbol asked for, which is a miss and not
                // a disagreement about the file.
                if named.is_ok() {
                    out.push(candidate(node, range, ancestors, file)?);
                }
            } else {
                named.with_context(|| {
                    format!(
                        "{}'s outline for {} advertises `{}`, but its range does not name it \
                         there; the outline does not describe the file as read from disk",
                        file.server,
                        file.path.display(),
                        node.extent.name
                    )
                })?;
                out.push(candidate(node, range, ancestors, file)?);
            }
        }
        ancestors.push((node.extent.name.as_str(), range));
        let descended = collect(&node.children, ancestors, file, out);
        ancestors.pop();
        descended?;
    }
    Ok(())
}

/// The range naming a flat-shape symbol, found on the first line of the
/// item the server reported.
///
/// Only that line is searched. A name recurs freely inside its own body —
/// a recursive call, `self.limit = limit` — and searching the whole item
/// would make such symbols ambiguous and so unresolvable. Every language
/// structural serves puts a declaration's name on its first line, so the
/// line is where the answer is and the rest is noise.
///
/// `None` when the line does not name the symbol, or names it twice: this
/// is nomination, and a nomination that cannot say which token it means is
/// not one (docs/correctness.md).
fn declaration_range(file: &NamedFile<'_>, node: &Node) -> Option<Range> {
    // In the decorated pass the advertised name is not written in the file
    // at all — erlang_ls advertises `choose/1` for a line that says
    // `choose(Flag) ->` — so the identifier is what the line is searched
    // for. The one-occurrence rule is unchanged either way.
    let name = if file.decorated && decorates(&node.extent.name, file.symbol) {
        file.symbol
    } else {
        node.extent.name.as_str()
    };
    let start = Position::new(node.extent.range.start.line, 0);
    let end = Position::new(node.extent.range.start.line, u32::MAX);
    let line = Range { start, end };
    // The item's first line, clamped to the file: `u32::MAX` is past every
    // line end, and `locate_name_at` would refuse a range outside the text.
    let line = clamp_to_line(file.text, line)?;
    match locate_name_at(file.text, line, name, file.encoding).ok()? {
        NameLocation::At(at) => Some(Range {
            start: at,
            end: Position::new(at.line, at.character + name.chars().count() as u32),
        }),
        NameLocation::Unnamed => None,
    }
}

/// `range` with its end pulled back to the real end of that line.
fn clamp_to_line(text: &str, range: Range) -> Option<Range> {
    let line = text.lines().nth(range.start.line as usize)?;
    Some(Range {
        start: range.start,
        end: Position::new(range.start.line, line.chars().count() as u32),
    })
}

/// The candidate an outline symbol is: its own name and kind, the range
/// that names it as the navigation point, and the container its enclosing
/// symbols name.
fn candidate(
    node: &Node,
    range: Range,
    ancestors: &[(&str, Range)],
    file: &NamedFile<'_>,
) -> Result<Candidate> {
    let name = file.identifier(node);
    // Nesting first: the hierarchical shape says the container by putting
    // the symbol under it, and the adapter's rule reads that. The flat
    // shape has no nesting and names the container in a field instead,
    // which is the same claim the symbol index makes and is qualified on
    // the same way (docs/resolution.md).
    let container_name = container(ancestors, file)?.or_else(|| node.extent.container_name.clone());
    // Built as the shape the servers report rather than field by field:
    // `SymbolInformation`'s `deprecated` field is itself deprecated in
    // lsp-types 0.97 (`tags` supersedes it) and still has to be provided.
    let info: SymbolInformation = serde_json::from_value(serde_json::json!({
        "name": name,
        "kind": node.extent.kind,
        "location": { "uri": file.uri.as_str(), "range": range },
        "containerName": container_name,
    }))
    .with_context(|| format!("shaping the candidate for `{name}`"))?;
    Ok(Candidate {
        server: file.server.to_string(),
        info,
        canonical_path: Some(file.path.to_path_buf()),
        exact: true,
    })
}

/// The container a qualified name filters on: the text at each enclosing
/// symbol's selection range, outermost first, joined the way a qualified
/// name is spelled (`nested::Deep`). The outermost symbol the file lists has
/// no container, which is the same fact the symbol index reports.
///
/// The text is checked against the enclosing node's advertised name: a
/// selection range names the symbol it belongs to, and for a structural node
/// that is a part of the advertised name (rust-analyzer selects `Config`
/// inside `impl Config`). A range that names something else no longer
/// belongs to the node it is claimed for, and is an error rather than a
/// wrong container on a qualifier.
fn container(ancestors: &[(&str, Range)], file: &NamedFile<'_>) -> Result<Option<String>> {
    if ancestors.is_empty() {
        return Ok(None);
    }
    let mut parts = Vec::with_capacity(ancestors.len());
    for (name, range) in ancestors {
        let selected = named_text(*range, file).with_context(|| {
            format!(
                "{}'s outline range for the symbol enclosing `{}` in {} names nothing readable",
                file.server,
                file.symbol,
                file.path.display()
            )
        })?;
        // The server's own rule decides what an enclosing node contributes:
        // its name for an ordinary symbol, and for rust-analyzer's impl node
        // the type the impl is written for. A shape that rule does not
        // recognize is an error — the container is never taken to be
        // whatever text the range happens to hold.
        //
        // The message states what was read and stops there. Two different
        // things bring it here — a range that does not name the node at
        // all, and one that names it more than once, which R's
        // `languageserver` does by selecting `Config <- setRefClass(`
        // through the `"Config"` argument on the next line — and naming
        // either as *the* cause would be a diagnosis this has not
        // established (docs/correctness.md).
        let container = file.adapter.outline_container(name, &selected).with_context(|| {
            format!(
                "{}'s outline names the symbol enclosing `{}` in {} as `{}`, and the text at its \
                 range is `{}`; that is not a pairing this server's outline rule recognizes, so \
                 what encloses it cannot be read",
                file.server,
                file.symbol,
                file.path.display(),
                name,
                crate::lsp::definition::elided(&selected)
            )
        })?;
        parts.push(container);
    }
    Ok(Some(parts.join("::")))
}

/// The text at `range`, from the file as read from disk.
fn named_text(range: Range, file: &NamedFile<'_>) -> Result<String> {
    let start = position_to_offset(file.text, range.start, file.encoding)
        .context("the range starts outside the file as read from disk")?;
    let end = position_to_offset(file.text, range.end, file.encoding)
        .context("the range ends outside the file as read from disk")?;
    if end < start {
        bail!("the range is inverted");
    }
    file.text
        .get(start..end)
        .map(str::to_string)
        .context("the range does not fall on the file's character boundaries")
}

#[cfg(test)]
mod tests;
