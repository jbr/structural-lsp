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

const RUST: &str = "/// Settings.
#[derive(Debug)]
pub struct Config {
    pub limit: usize,
}

impl Config {
    /// Double the limit.
    #[inline]
    pub fn doubled(&self) -> usize {
        self.limit * 2
    }
}
";

/// An extent as `documentSymbol` gives one: from the start of `from` to
/// the end of `through`.
fn extent(source: &str, from: &str, through: &str) -> (usize, usize) {
    let start = source.find(from).expect("start present");
    let end = source.find(through).expect("end present") + through.len();
    (start, end)
}

#[test]
fn an_indented_item_keeps_the_indentation_of_its_first_line() {
    // The server's extent starts at `pub`, four columns in; widened over
    // the doc comment and attribute, the body's first line is indented as
    // its later lines are, the way the file has it.
    let (start, end) = extent(RUST, "pub fn doubled", "self.limit * 2\n    }");
    let body = extent_body("rust", RUST, start, end).unwrap();
    assert_eq!(
        body,
        "    /// Double the limit.\n    #[inline]\n    pub fn doubled(&self) -> usize {\n        self.limit * 2\n    }",
    );
    // An extent that already covers the decorations (rust-analyzer's covers
    // a leading comment) starts at the same column: the same body.
    let (start, end) = extent(RUST, "/// Double", "self.limit * 2\n    }");
    assert_eq!(extent_body("rust", RUST, start, end).unwrap(), body);
}

#[test]
fn a_top_level_item_is_cut_where_its_decorations_start() {
    let (start, end) = extent(RUST, "pub struct Config", "usize,\n}");
    assert_eq!(
        extent_body("rust", RUST, start, end).unwrap(),
        "/// Settings.\n#[derive(Debug)]\npub struct Config {\n    pub limit: usize,\n}",
    );
}

#[test]
fn an_indented_python_method_keeps_its_decorators_indentation() {
    let source = "class Box:\n    @property\n    def size(self):\n        return 1\n";
    let (start, end) = extent(source, "def size", "return 1");
    assert_eq!(
        extent_body("python", source, start, end).unwrap(),
        "    @property\n    def size(self):\n        return 1",
    );
}

#[test]
fn an_item_without_a_grammar_keeps_its_indentation_too() {
    let source = "type Config struct {\n\tLimit int\n}\n";
    let (start, end) = extent(source, "Limit", "Limit int");
    assert_eq!(extent_body("go", source, start, end).unwrap(), "\tLimit int");
}

#[test]
fn a_start_with_code_before_it_on_its_line_is_not_widened() {
    // Two items on one line: the second's body is its own text, not the
    // first's tail.
    let source = "struct Marker; fn after() {}\n";
    let (start, end) = extent(source, "fn after", "{}");
    assert_eq!(
        extent_body("rust", source, start, end).unwrap(),
        "fn after() {}"
    );
}

#[test]
fn a_fallback_full_range_keeps_the_indentation_of_its_first_line() {
    let before = &RUST[..RUST.find("pub fn doubled").unwrap()];
    let signature = before.matches('\n').count() as u32;
    let body = fallback_body(
        RUST,
        &target(
            Some(range((signature, 4), (signature + 2, 5))),
            range((signature, 11), (signature, 18)),
        ),
        Encoding::Utf16,
    )
    .expect("the full range is usable");
    assert_eq!(
        body,
        "    pub fn doubled(&self) -> usize {\n        self.limit * 2\n    }",
    );
}
