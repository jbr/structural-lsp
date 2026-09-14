use super::*;
use crate::lsp::positions::Encoding;
use crate::lsp::definition::NavigationProvenance;
use lsp_types::Uri;

fn target(
    full_range: Option<lsp_types::Range>,
    selection: lsp_types::Range,
) -> DefinitionTarget {
    let uri: Uri = serde_json::from_value(serde_json::json!("file:///ws/src/lib.rs")).unwrap();
    DefinitionTarget {
        uri,
        selection_range: selection,
        full_range,
        provenance: NavigationProvenance::LocationRange,
    }
}

fn range(start: (u32, u32), end: (u32, u32)) -> lsp_types::Range {
    lsp_types::Range {
        start: lsp_types::Position {
            line: start.0,
            character: start.1,
        },
        end: lsp_types::Position {
            line: end.0,
            character: end.1,
        },
    }
}

const TEXT: &str = "first line\nsecond line\nthird line\n";

#[test]
fn fallback_prefers_the_whole_full_range() {
    // full_range spans both lines; the selection range is a strict
    // subset. The body must come from the full range, not the selection.
    let body = fallback_body(
        TEXT,
        &target(Some(range((0, 0), (1, 11))), range((1, 0), (1, 6))),
        Encoding::Utf16,
    )
    .expect("the full range is usable");
    assert_eq!(body, "first line\nsecond line");
}

#[test]
fn fallback_with_an_unusable_full_range_is_none_not_a_mixed_span() {
    // The full range's END points past the file (a stale/invalid range);
    // mixing its start with the selection's end would render an invented
    // span, and mixing with its start would be the old bug. Atomicity:
    // the whole full range is unusable, so nothing is rendered.
    let body = fallback_body(
        TEXT,
        &target(Some(range((0, 0), (99, 0))), range((1, 0), (1, 6))),
        Encoding::Utf16,
    );
    assert_eq!(body, None);
}

#[test]
fn fallback_without_a_full_range_uses_the_selection_range() {
    let body = fallback_body(TEXT, &target(None, range((1, 0), (1, 6))), Encoding::Utf16)
        .expect("the selection range is usable");
    assert_eq!(body, "second");
}

#[test]
fn fallback_with_an_unusable_selection_range_is_none() {
    let body = fallback_body(TEXT, &target(None, range((50, 0), (51, 6))), Encoding::Utf16);
    assert_eq!(body, None);
}

#[test]
fn fallback_with_an_inverted_range_is_none() {
    let body = fallback_body(TEXT, &target(None, range((1, 6), (1, 0))), Encoding::Utf16);
    assert_eq!(body, None, "end before start is not a body");
}
