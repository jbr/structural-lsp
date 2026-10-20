use super::locals::{size, strip, strip_below};
use super::page::{Line, PAGE_LINES, count, page};
use crate::lsp::outline::{Extent, Node};
use lsp_types::{Position, Range, SymbolKind};

fn node(name: &str, kind: SymbolKind, children: Vec<Node>) -> Node {
    let at = Position::new(0, 0);
    Node {
        extent: Extent {
            name: name.to_string(),
            kind,
            range: Range::new(at, at),
            selection_range: None,
            detail: None,
            container_name: None,
        },
        children,
    }
}

fn names(nodes: &[Node]) -> Vec<String> {
    fn walk(nodes: &[Node], depth: usize, out: &mut Vec<String>) {
        for node in nodes {
            out.push(format!("{}{}", "  ".repeat(depth), node.extent.name));
            walk(&node.children, depth + 1, out);
        }
    }
    let mut out = Vec::new();
    walk(nodes, 0, &mut out);
    out
}

/// pyright's shape: parameters and locals as `Variable` children of a
/// `def`, class and instance attributes as `Variable` children of a class,
/// module variables at the top.
#[test]
fn a_functions_locals_leave_and_a_class_or_modules_variables_stay() {
    use SymbolKind as K;
    let mut nodes = vec![
        node("TOP", K::CONSTANT, vec![]),
        node(
            "Config",
            K::CLASS,
            vec![
                node("attr", K::VARIABLE, vec![]),
                node(
                    "method",
                    K::METHOD,
                    vec![
                        node("x", K::VARIABLE, vec![]),
                        node("inner", K::FUNCTION, vec![node("y", K::VARIABLE, vec![])]),
                    ],
                ),
                node("limit", K::VARIABLE, vec![]),
            ],
        ),
        node(
            "free",
            K::FUNCTION,
            vec![
                node("a", K::VARIABLE, vec![]),
                node("MAX", K::CONSTANT, vec![]),
            ],
        ),
    ];
    let removed = strip(&mut nodes, None);
    assert_eq!(removed, 4);
    assert_eq!(
        names(&nodes),
        [
            "TOP",
            "Config",
            "  attr",
            "  method",
            "    inner",
            "  limit",
            "free"
        ]
    );
}

/// vtsls's shape: a local object's properties go with it, and an arrow
/// function bound to a module `const` keeps the `const` and drops its
/// locals.
#[test]
fn a_locals_subtree_goes_with_it_and_a_bound_functions_locals_leave() {
    use SymbolKind as K;
    let mut nodes = vec![
        node(
            "method",
            K::METHOD,
            vec![node(
                "obj",
                K::VARIABLE,
                vec![
                    node("a", K::PROPERTY, vec![]),
                    node("b", K::METHOD, vec![]),
                ],
            )],
        ),
        node("handler", K::VARIABLE, vec![node("z", K::VARIABLE, vec![])]),
    ];
    assert_eq!(size(&nodes[0]), 4);
    let removed = strip(&mut nodes, None);
    assert_eq!(removed, 4);
    assert_eq!(names(&nodes), ["method", "handler"]);
}

/// A function's own outline lists its locals; theirs below it do not.
#[test]
fn the_targets_own_children_stay() {
    use SymbolKind as K;
    let mut children = vec![
        node("x", K::VARIABLE, vec![]),
        node("inner", K::FUNCTION, vec![node("y", K::VARIABLE, vec![])]),
    ];
    assert_eq!(strip_below(&mut children), 1);
    assert_eq!(names(&children), ["x", "inner"]);
}

fn lines(depths: &[usize]) -> Vec<Line> {
    depths
        .iter()
        .enumerate()
        .map(|(at, &depth)| Line {
            depth,
            text: format!("l{at}"),
        })
        .collect()
}

#[test]
fn a_page_that_starts_inside_a_subtree_is_preceded_by_its_ancestors() {
    // l0 at depth 0, l1 at depth 1, then depth-2 lines past the page edge.
    let mut depths = vec![0, 1];
    depths.extend(std::iter::repeat_n(2, PAGE_LINES + 5));
    let all = lines(&depths);
    let first = page(&all, 1, false).unwrap();
    assert!(first.context.is_empty());
    assert_eq!(first.lines.len(), PAGE_LINES);
    assert!(first.more);
    assert_eq!(first.label(Some(count(all.len()))), "");
    assert_eq!(
        first.continuation("7 more symbol(s)"),
        "note: 7 more symbol(s); `page: 2` continues\n"
    );

    let second = page(&all, 2, false).unwrap();
    assert_eq!(second.lines.len(), 7);
    assert!(!second.more);
    assert_eq!(second.continuation("nothing"), "");
    assert_eq!(second.label(Some(count(all.len()))), " · page 2 of 2");
    let mut text = String::new();
    second.render(&mut text);
    let rendered: Vec<&str> = text.lines().take(3).collect();
    assert_eq!(rendered, ["l0", "  l1", &format!("    l{PAGE_LINES}")]);
}

#[test]
fn a_page_past_the_end_or_numbered_zero_is_refused() {
    let all = lines(&[0, 0, 0]);
    let error = page(&all, 2, false).unwrap_err().to_string();
    assert_eq!(error, "`page: 2` is past the end: this outline has 1 page(s)");
    assert_eq!(
        page(&all, 0, false).unwrap_err().to_string(),
        "`page` counts from 1"
    );
    // A caller that has not produced every line yet still gets a
    // continuation when this page is exactly full.
    let full = lines(&vec![0; PAGE_LINES]);
    assert!(!page(&full, 1, false).unwrap().more);
    assert!(page(&full, 1, true).unwrap().more);
    assert_eq!(page(&full, 1, true).unwrap().label(None), "");
    // An empty outline is one page, and page 1 of it is not an error.
    assert!(page(&[], 1, false).unwrap().lines.is_empty());
}
