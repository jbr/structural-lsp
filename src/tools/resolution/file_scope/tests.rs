//! Unit tests for the outline walk: what a file's own `documentSymbol`
//! tree nominates, and what it names a symbol's container.

use super::*;
use crate::lsp::adapter::{pyright::Pyright, rust_analyzer::RustAnalyzer};
use crate::tools::resolution::qualify;
use lsp_types::Position;
use serde_json::json;

/// The text every range below is checked against: an inherent impl under a
/// `struct`, a trait impl, and a nested module, the shapes rust-analyzer
/// nests (§ the raw shape in the module doc).
const TEXT: &str = "\
struct Item {
}
impl Item {
    fn new() {}
}
pub mod nested {
    struct Deep {
    }
    impl Deep {
        fn new() {}
    }
}
";

fn range(sl: u32, sc: u32, el: u32, ec: u32) -> Value {
    json!({ "start": { "line": sl, "character": sc }, "end": { "line": el, "character": ec } })
}

/// rust-analyzer's shape: the struct and each `impl` are siblings, methods
/// are children of the `impl`, and every node's selectionRange names it —
/// for an `impl`, the type it is written for.
fn outline() -> Value {
    json!([
        {
            "name": "Item", "kind": 23,
            "range": range(0, 0, 1, 1), "selectionRange": range(0, 7, 0, 11),
            "detail": null
        },
        {
            "name": "impl Item", "kind": 19,
            "range": range(2, 0, 4, 1), "selectionRange": range(2, 5, 2, 9),
            "children": [
                {
                    "name": "new", "kind": 12,
                    "range": range(3, 4, 3, 15), "selectionRange": range(3, 7, 3, 10),
                    "detail": "fn()"
                }
            ]
        },
        {
            "name": "nested", "kind": 2,
            "range": range(5, 0, 11, 1), "selectionRange": range(5, 8, 5, 14),
            "children": [
                {
                    "name": "Deep", "kind": 23,
                    "range": range(6, 4, 7, 5), "selectionRange": range(6, 11, 6, 15)
                },
                {
                    "name": "impl Deep", "kind": 19,
                    "range": range(8, 4, 10, 5), "selectionRange": range(8, 9, 8, 13),
                    "children": [
                        {
                            "name": "new", "kind": 12,
                            "range": range(9, 8, 9, 19), "selectionRange": range(9, 11, 9, 14)
                        }
                    ]
                }
            ]
        }
    ])
}

fn find(symbol: &str) -> Vec<Candidate> {
    find_in(&RustAnalyzer, &outline(), TEXT, symbol).unwrap()
}

/// The walk as a tool runs it: one server's outline, the text it was read
/// from, and the adapter whose outline rules that server follows.
fn find_in(
    adapter: &dyn Adapter,
    outline: &Value,
    text: &str,
    symbol: &str,
) -> Result<Vec<Candidate>> {
    let uri: Uri = serde_json::from_value(Value::String("file:///ws/src/a.rs".to_string()))
        .expect("a literal URI parses");
    let file = NamedFile {
        text,
        encoding: Encoding::Utf8,
        symbol,
        server: adapter.name(),
        adapter,
        uri,
        path: Path::new("/ws/src/a.rs"),
    };
    candidates(outline, &file)
}

fn at(line: u32, character: u32) -> Position {
    Position::new(line, character)
}

#[test]
fn every_exact_name_in_the_file_is_a_candidate_with_its_container() {
    let found = find("new");
    assert_eq!(found.len(), 2, "both `new`s, not just the first");
    let innermost = &found[0];
    assert_eq!(innermost.info.name, "new");
    assert!(innermost.exact);
    assert_eq!(
        innermost.info.container_name.as_deref(),
        Some("Item"),
        "the container is the text at the impl's selection range, not its node name"
    );
    assert_eq!(
        innermost.info.location.range,
        Range::new(at(3, 7), at(3, 10)),
        "the navigation point is the range that names the symbol"
    );
    assert_eq!(
        innermost.canonical_path.as_deref(),
        Some(Path::new("/ws/src/a.rs"))
    );
    assert_eq!(innermost.server, "rust-analyzer");
    assert_eq!(innermost.info.location.uri.as_str(), "file:///ws/src/a.rs");
    let deep = &found[1];
    assert_eq!(deep.info.container_name.as_deref(), Some("nested::Deep"));
}

#[test]
fn a_qualifier_the_server_never_reported_can_still_match_the_file() {
    let containers: Vec<Option<String>> = find("new")
        .iter()
        .map(|candidate| candidate.info.container_name.clone())
        .collect();
    let matches = |qualifier: &[&str]| {
        let qualifier = qualifier
            .iter()
            .map(|part| part.to_string())
            .collect::<Vec<_>>();
        containers
            .iter()
            .filter(|container| qualify::container_matches(container.as_deref(), &qualifier))
            .count()
    };
    assert_eq!(matches(&["Item"]), 1);
    assert_eq!(matches(&["nested", "Deep"]), 1, "a module path is a suffix");
    assert_eq!(matches(&["Deep"]), 1);
    assert_eq!(matches(&["Missing"]), 0);
}

#[test]
fn the_advertised_name_is_the_identity_and_the_range_is_only_the_navigation_point() {
    // `Item` is the struct; the impl node advertises `impl Item` and is not
    // a candidate for the type it is written for.
    let found = find("Item");
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].info.name, "Item");
    assert_eq!(found[0].info.kind, lsp_types::SymbolKind::STRUCT);
    assert_eq!(found[0].info.container_name, None);
    assert_eq!(
        found[0].info.location.range,
        Range::new(at(0, 7), at(0, 11)),
        "the range that names the symbol is the navigation point"
    );
    // A nested declaration's container is the module's node, whose range
    // names the module itself.
    let deep = find("Deep");
    assert_eq!(deep.len(), 1);
    assert_eq!(deep[0].info.container_name.as_deref(), Some("nested"));
}

#[test]
fn an_advertised_name_that_is_not_at_its_range_is_an_error_before_any_listing() {
    // `impl Item` is what the server advertises; its range names `Item`. The
    // outline is not describing the file, and a candidate built on it would be
    // a listing or a resolution from a range that says something else.
    let error = find_in(&RustAnalyzer, &outline(), TEXT, "impl Item").unwrap_err();
    let text = format!("{error:#}");
    assert!(text.contains("advertises `impl Item`"), "{text}");
    assert!(
        text.contains("does not describe the file as read from disk"),
        "{text}"
    );
    assert!(text.contains("found `Item`"), "{text}");
}

#[test]
fn a_longer_ordinary_name_cannot_pass_off_its_tail_as_a_container() {
    // The node is a class-like declaration advertised `OtherItem`; its stale
    // selection range names `Item`. Substring containment would have made
    // `Item` a container and let a `Item::new` qualifier select a symbol in
    // `OtherItem` — the ordinary rule requires the range to name the node.
    let collision = json!([
        {
            "name": "OtherItem", "kind": 23,
            "range": range(0, 0, 1, 1), "selectionRange": range(0, 7, 0, 11),
            "children": [
                {
                    "name": "new", "kind": 12,
                    "range": range(3, 4, 3, 15), "selectionRange": range(3, 7, 3, 10)
                }
            ]
        }
    ]);
    let error = find_in(&RustAnalyzer, &collision, TEXT, "new").unwrap_err();
    let text = format!("{error:#}");
    assert!(text.contains("`OtherItem`"), "{text}");
    assert!(text.contains("`Item`"), "{text}");
    assert!(
        !text.contains("container"),
        "no container is produced: {text}"
    );
    // The same collision through an impl node's own name, where substring
    // containment would also have accepted it.
    let impl_collision = json!([
        {
            "name": "impl OtherItem", "kind": 19,
            "range": range(2, 0, 4, 1), "selectionRange": range(2, 5, 2, 9),
            "children": [
                {
                    "name": "new", "kind": 12,
                    "range": range(3, 4, 3, 15), "selectionRange": range(3, 7, 3, 10)
                }
            ]
        }
    ]);
    assert!(find_in(&RustAnalyzer, &impl_collision, TEXT, "new").is_err());
}

#[test]
fn an_enclosing_range_that_does_not_belong_to_its_node_is_an_error_not_a_container() {
    // The node advertises `impl Other` while its range names `Item`: the
    // range no longer belongs to the node it is claimed for, so the container
    // a qualifier would filter on cannot be trusted and is not guessed.
    let stale = json!([
        {
            "name": "impl Other", "kind": 19,
            "range": range(2, 0, 4, 1), "selectionRange": range(2, 5, 2, 9),
            "children": [
                {
                    "name": "new", "kind": 12,
                    "range": range(3, 4, 3, 15), "selectionRange": range(3, 7, 3, 10)
                }
            ]
        }
    ]);
    let error = find_in(&RustAnalyzer, &stale, TEXT, "new").unwrap_err();
    let text = format!("{error:#}");
    assert!(text.contains("`impl Other`"), "{text}");
    assert!(text.contains("`Item`"), "{text}");
    // What was advertised and what was read, and no claim about which is
    // wrong: a range naming nothing and a range naming the node twice both
    // arrive here, and the message that named a cause named it wrongly for
    // one of them (docs/correctness.md).
    assert!(
        text.contains("not a pairing this server's outline rule recognizes"),
        "{text}"
    );
}

/// The flat `SymbolInformation[]` shape has no range naming each symbol,
/// so its declaration line is searched for the advertised name. pylsp
/// 1.15.0 serves this shape, and before this a file scope could nominate
/// nothing from it (`backlog/symbol-range-convention.md`).
#[test]
fn a_flat_outline_nominates_from_the_declaration_line() {
    // `location.range` spans the whole item; line 3 is `    fn new() {}`.
    let flat = json!([
        {
            "name": "new", "kind": 12,
            "location": { "uri": "file:///ws/src/a.rs", "range": range(3, 0, 3, 15) },
            "containerName": "Item"
        }
    ]);
    let found = find_in(&RustAnalyzer, &flat, TEXT, "new").unwrap();
    assert_eq!(found.len(), 1, "the declaration line names `new`");
    // The range handed on is the name's, not the item's: it is where the
    // pipeline sends `definition`.
    assert_eq!(
        found[0].info.location.range,
        lsp_types::Range {
            start: lsp_types::Position::new(3, 7),
            end: lsp_types::Position::new(3, 10),
        },
        "the nominated range is the name"
    );

    let null = find_in(&RustAnalyzer, &Value::Null, TEXT, "new").unwrap();
    assert!(null.is_empty());
}

#[test]
fn a_flat_symbol_whose_line_does_not_name_it_nominates_nothing() {
    // Line 0 is `struct Item {`, which does not hold `new`. A server and a
    // file that disagree this way nominate nothing and leave the symbol
    // index as the fallback, rather than guessing at a token.
    let wrong_line = json!([
        {
            "name": "new", "kind": 12,
            "location": { "uri": "file:///ws/src/a.rs", "range": range(0, 0, 0, 13) },
            "containerName": "Item"
        }
    ]);
    assert!(
        find_in(&RustAnalyzer, &wrong_line, TEXT, "new")
            .unwrap()
            .is_empty()
    );
}

/// Measured rust-analyzer shapes: an inherent impl, a trait impl and a
/// generic impl, each selecting the type it is written for.
const SHAPES_TEXT: &str = "\
struct Item {
}
impl Item {
    fn new() {}
}
impl Label for Item {
    fn label() {}
}
struct Wrapper<T> {
    value: T,
}
impl<T> Wrapper<T> {
    fn new() {}
}
";

fn shapes_outline() -> Value {
    json!([
        {
            "name": "Item", "kind": 23,
            "range": range(0, 0, 1, 1), "selectionRange": range(0, 7, 0, 11)
        },
        {
            "name": "impl Item", "kind": 19,
            "range": range(2, 0, 4, 1), "selectionRange": range(2, 5, 2, 9),
            "children": [
                {
                    "name": "new", "kind": 12,
                    "range": range(3, 4, 3, 14), "selectionRange": range(3, 7, 3, 10)
                }
            ]
        },
        {
            "name": "impl Label for Item", "kind": 19,
            "range": range(5, 0, 7, 1), "selectionRange": range(5, 15, 5, 19),
            "children": [
                {
                    "name": "label", "kind": 6,
                    "range": range(6, 4, 6, 17), "selectionRange": range(6, 7, 6, 12)
                }
            ]
        },
        {
            "name": "Wrapper", "kind": 23,
            "range": range(8, 0, 10, 1), "selectionRange": range(8, 7, 8, 14)
        },
        {
            "name": "impl<T> Wrapper<T>", "kind": 19,
            "range": range(11, 0, 13, 1), "selectionRange": range(11, 8, 11, 18),
            "children": [
                {
                    "name": "new", "kind": 12,
                    "range": range(12, 4, 12, 14), "selectionRange": range(12, 7, 12, 10)
                }
            ]
        }
    ])
}

#[test]
fn generic_and_trait_impl_containers_are_the_types_their_ranges_name() {
    let found = find_in(&RustAnalyzer, &shapes_outline(), SHAPES_TEXT, "new").unwrap();
    let containers: Vec<&str> = found
        .iter()
        .filter_map(|candidate| candidate.info.container_name.as_deref())
        .collect();
    assert_eq!(
        containers,
        ["Item", "Wrapper<T>"],
        "an inherent impl and a generic one, the type name kept whole"
    );
    let trait_impl = find_in(&RustAnalyzer, &shapes_outline(), SHAPES_TEXT, "label").unwrap();
    assert_eq!(trait_impl.len(), 1);
    assert_eq!(
        trait_impl[0].info.container_name.as_deref(),
        Some("Item"),
        "`impl Label for Item` containers its method as `Item`, the spelling the index reports"
    );
    assert_eq!(
        find_in(&RustAnalyzer, &shapes_outline(), SHAPES_TEXT, "Wrapper")
            .unwrap()
            .len(),
        1
    );
}

/// A class and a method, the shape pyright's outline nests.
const PY_TEXT: &str = "\
class Config:
    def doubled(self):
        return 0
";

fn py_outline() -> Value {
    json!([
        {
            "name": "Config", "kind": 5,
            "range": range(0, 0, 2, 13), "selectionRange": range(0, 6, 0, 12),
            "children": [
                {
                    "name": "doubled", "kind": 6,
                    "range": range(1, 4, 2, 13), "selectionRange": range(1, 8, 1, 15)
                }
            ]
        }
    ])
}

#[test]
fn a_python_methods_container_is_its_class_node() {
    let found = find_in(&Pyright, &py_outline(), PY_TEXT, "doubled").unwrap();
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].info.container_name.as_deref(), Some("Config"));
    assert_eq!(
        found[0].info.location.range,
        Range::new(at(1, 8), at(1, 15))
    );
}

#[test]
fn an_enclosing_range_that_names_nothing_is_an_error_not_a_guess() {
    let broken = json!([
        {
            "name": "impl Item", "kind": 19,
            "range": range(2, 0, 4, 1), "selectionRange": range(90, 0, 90, 4),
            "children": [
                {
                    "name": "new", "kind": 12,
                    "range": range(3, 4, 3, 15), "selectionRange": range(3, 7, 3, 10)
                }
            ]
        }
    ]);
    let error = find_in(&RustAnalyzer, &broken, TEXT, "new").unwrap_err();
    let text = format!("{error:#}");
    assert!(text.contains("names nothing readable"), "{text}");
    assert!(text.contains("src/a.rs"), "{text}");
}

#[test]
fn an_unrecognizable_outline_is_an_error() {
    let error = find_in(&RustAnalyzer, &json!({ "x": 1 }), TEXT, "new").unwrap_err();
    assert!(
        format!("{error:#}").contains("unusable documentSymbol result"),
        "{error:#}"
    );
}
