//! Rendering for `show`'s body path: the header line, the body, the notes,
//! and the fallback/budget helpers. Listings are shared in
//! [`crate::tools::listing`].
//!
//! Every byte is paid for on every call. The happy path is one header line
//! and the body; notes appear only when something is degraded, one line
//! each.

use super::*;
use crate::lsp::positions::Encoding;

/// The happy path: the header line, the body, then any notes.
pub(super) fn render_resolved(header: &str, body: &str, notes: &[String]) -> String {
    let mut text = format!("{header}\n{body}\n");
    for note in notes {
        text.push_str(&format!("note: {note}\n"));
    }
    text
}

/// The body for an item whose extent is `start..end` of `text`: the start
/// widened over the item's leading decorations (docs/resolution.md), then
/// back to the start of its line ([`from_line_start`]).
pub(super) fn extent_body<'a>(
    language: &str,
    text: &'a str,
    start: usize,
    end: usize,
) -> Result<&'a str> {
    let start = shape::extend_start(language, text, start)?;
    Ok(from_line_start(text, start, end))
}

/// The body `start..end` of `text`, begun at the start of its first line
/// when only spaces and tabs precede `start` there. A server's range, and
/// a range widened over decorations, starts at its first token, so an
/// indented item cut there loses its first line's indentation while every
/// later line keeps its own. Nothing is dedented: each line of a body reads
/// as it does in the file. A start with other text before it on its line
/// stays where it is.
pub(super) fn from_line_start(text: &str, start: usize, end: usize) -> &str {
    let before = text[..start].trim_end_matches([' ', '\t']);
    let start = if before.is_empty() || before.ends_with('\n') {
        before.len()
    } else {
        start
    };
    &text[start..end]
}

/// The raw body from the server's own ranges when shaping has no answer: the
/// full range when the server gave one, else the selection range — chosen as
/// a whole. An endpoint of one range is never mixed with an endpoint of the
/// other (a half-shifted pair would render an invented span); an unusable
/// range yields nothing. A range indented on its line keeps the indentation
/// ([`from_line_start`]).
pub(super) fn fallback_body(
    target_text: &str,
    target: &DefinitionTarget,
    encoding: Encoding,
) -> Option<String> {
    let (start, end) = match target.full_range {
        Some(range) => (
            position_to_offset(target_text, range.start, encoding).ok()?,
            position_to_offset(target_text, range.end, encoding).ok()?,
        ),
        None => (
            position_to_offset(target_text, target.selection_range.start, encoding).ok()?,
            position_to_offset(target_text, target.selection_range.end, encoding).ok()?,
        ),
    };
    if end > start {
        Some(from_line_start(target_text, start, end).to_string())
    } else {
        None
    }
}

/// Trim a body to the budget on a character boundary. The note says what was
/// cut; the body itself carries no marker.
pub(super) fn budget(body: &str) -> (String, Option<String>) {
    if body.len() <= BODY_BUDGET_BYTES {
        return (body.to_string(), None);
    }
    let mut end = BODY_BUDGET_BYTES;
    while !body.is_char_boundary(end) {
        end -= 1;
    }
    (
        body[..end].to_string(),
        Some(format!(
            "truncated after {end} of {} bytes (result budget)",
            body.len()
        )),
    )
}
