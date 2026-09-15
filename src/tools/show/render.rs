//! Rendering for `show`'s body path: the header line, the body, the notes,
//! and the fallback/budget helpers. Listings are shared in
//! [`crate::tools::listing`].
//!
//! Every byte is paid for on every call. The happy path is one header line
//! and the body; notes appear only when something is degraded, one line
//! each.

use super::*;
use crate::lsp::positions::Encoding;

/// The happy path: `kind name · path:line`, the body, then any notes.
pub(super) fn render_resolved(
    root: &Path,
    kind_label: &str,
    name: &str,
    location: &lsp_types::Location,
    body: &str,
    notes: &[String],
) -> String {
    let mut text = format!(
        "{kind_label} {name} · {}\n{body}\n",
        location_line(root, &location.uri, location.range.start)
    );
    for note in notes {
        text.push_str(&format!("note: {note}\n"));
    }
    text
}

/// The raw body from the server's own ranges when shaping has no answer: the
/// full range when the server gave one, else the selection range — chosen as
/// a whole. An endpoint of one range is never mixed with an endpoint of the
/// other (a half-shifted pair would render an invented span); an unusable
/// range yields nothing.
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
        Some(target_text[start..end].to_string())
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
