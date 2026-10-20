//! What a candidate's file outline encloses it in, for a candidate whose
//! server reported no container.
//!
//! rust-analyzer's `workspace/symbol` reports the impl type as a method's
//! container and nothing for a free function — even one inside `mod tests`
//! — while its `documentSymbol` nests the function under the module. The
//! outline is the same statement a `path` naming the file reads its
//! containers from (`file_scope`), so asking it here makes a qualifier mean
//! the same thing with and without that `path` (`docs/resolution.md`).

use super::Qualified;
use crate::lsp::definition::{Candidate, NameLocation, locate_name_at};
use crate::tools::listing::location_line;
use crate::tools::resolution::{file_scope, server_named};
use crate::workspace::Workspace;
use anyhow::{Context, Result};
use std::path::Path;

/// Whether the candidate's file, as on disk, contains the qualifier's last
/// segment at all.
///
/// Every container an outline contributes is text at a range in the file
/// (`Adapter::outline_container`), and a nesting confirms a qualifier only
/// by ending with its last segment, so a file without that text cannot
/// confirm it and its outline is not asked for. A file that cannot be read
/// is not asked either; the navigate step reports it if it is kept.
pub fn mentioned(candidate: &Candidate, qualified: &Qualified) -> bool {
    let (Some(file), Some(last)) = (&candidate.canonical_path, qualified.qualifiers.last()) else {
        return false;
    };
    std::fs::read_to_string(file).is_ok_and(|text| text.contains(last.as_str()))
}

/// The container the candidate's file outline names for it: the outline
/// symbol with the queried name whose own range starts where the
/// candidate's name sits.
///
/// The file is synced and its outline read the way a file scope reads it,
/// with every range checked against the text just read. The candidate's
/// own range is checked too, as the navigate step would: a range that no
/// longer names the symbol is a stale index and an error, never a candidate
/// quietly left out of the filter while another is picked
/// (`docs/correctness.md`). `None` when the outline lists no such symbol,
/// lists several at that position, or places it at the top of the file —
/// none of those is a container.
pub fn nesting_of(
    state: &mut Workspace,
    root: &Path,
    candidate: &Candidate,
    name: &str,
) -> Result<Option<String>> {
    let Some(file) = candidate.canonical_path.as_deref() else {
        return Ok(None);
    };
    let server = server_named(state, &candidate.server)?;
    let encoding = server.encoding();
    let (text, outlined) = file_scope::outline_of(server, file, name)?;
    let range = candidate.info.location.range;
    let at = locate_name_at(&text, range, name, encoding).with_context(|| {
        format!(
            "{}'s location for '{name}' ({}) does not name it in the file on disk",
            candidate.server,
            location_line(root, &candidate.info.location.uri, range.start)
        )
    })?;
    let NameLocation::At(position) = at else {
        return Ok(None);
    };
    let mut there = outlined
        .into_iter()
        .filter(|symbol| symbol.info.location.range.start == position);
    Ok(match (there.next(), there.next()) {
        (Some(symbol), None) => symbol.info.container_name,
        _ => None,
    })
}
