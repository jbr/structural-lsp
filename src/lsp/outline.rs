//! `textDocument/documentSymbol` as an outline: the innermost symbol whose
//! range contains a position gives an item's extent and kind, for any
//! server, without a grammar.
//!
//! Both result shapes are handled: the hierarchical `DocumentSymbol[]`
//! (children nested, which is what every server here returns given
//! `hierarchicalDocumentSymbolSupport`) and the flat `SymbolInformation[]`
//! (no nesting; the smallest containing range wins).

use anyhow::{Context, Result};
use lsp_types::{DocumentSymbol, Position, Range, SymbolInformation, SymbolKind};
use serde_json::Value;

/// One symbol's extent from the outline.
#[derive(Debug, Clone, PartialEq)]
pub struct Extent {
    pub name: String,
    pub kind: SymbolKind,
    /// The whole item, as the server sees it.
    pub range: Range,
    /// The range of the symbol's own name, when the result shape carries
    /// one (`DocumentSymbol` does; `SymbolInformation` does not).
    pub selection_range: Option<Range>,
    /// The server's one-line detail (rust-analyzer: the signature), when
    /// the result shape carries one.
    pub detail: Option<String>,
    /// The container the server itself named, which only the flat
    /// `SymbolInformation` shape carries. The hierarchical shape says the
    /// same thing by nesting, so this is `None` there and the enclosing
    /// nodes are read instead.
    pub container_name: Option<String>,
}

/// The innermost symbol whose range contains `position`, or `None` when no
/// symbol does (a position between items, or an empty outline). `Err` only
/// for a result that is neither shape.
pub fn innermost_at(result: &Value, position: Position) -> Result<Option<Extent>> {
    if result.is_null() {
        return Ok(None);
    }
    let Some(items) = result.as_array() else {
        anyhow::bail!("documentSymbol result is neither null nor an array: {result}");
    };
    if items.is_empty() {
        return Ok(None);
    }
    // `DocumentSymbol` has `range`; `SymbolInformation` has `location`.
    if items[0].get("range").is_some() {
        let symbols: Vec<DocumentSymbol> =
            serde_json::from_value(result.clone()).context("parsing DocumentSymbol[]")?;
        Ok(innermost_hierarchical(&symbols, position))
    } else {
        let symbols: Vec<SymbolInformation> =
            serde_json::from_value(result.clone()).context("parsing SymbolInformation[]")?;
        Ok(innermost_flat(&symbols, position))
    }
}

/// An outline symbol with its children, for tree views.
#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    pub extent: Extent,
    pub children: Vec<Node>,
}

/// The whole outline as a tree. The flat shape has no nesting, so every
/// symbol is a root in the server's order.
pub fn tree(result: &Value) -> Result<Vec<Node>> {
    if result.is_null() {
        return Ok(Vec::new());
    }
    let Some(items) = result.as_array() else {
        anyhow::bail!("documentSymbol result is neither null nor an array: {result}");
    };
    if items.is_empty() {
        return Ok(Vec::new());
    }
    if items[0].get("range").is_some() {
        let symbols: Vec<DocumentSymbol> =
            serde_json::from_value(result.clone()).context("parsing DocumentSymbol[]")?;
        Ok(nodes(&symbols))
    } else {
        let symbols: Vec<SymbolInformation> =
            serde_json::from_value(result.clone()).context("parsing SymbolInformation[]")?;
        Ok(symbols
            .iter()
            .map(|symbol| Node {
                extent: Extent {
                    name: symbol.name.clone(),
                    kind: symbol.kind,
                    range: symbol.location.range,
                    selection_range: None,
                    detail: None,
                    container_name: symbol.container_name.clone(),
                },
                children: Vec::new(),
            })
            .collect())
    }
}

fn nodes(symbols: &[DocumentSymbol]) -> Vec<Node> {
    symbols
        .iter()
        .map(|symbol| Node {
            extent: Extent {
                name: symbol.name.clone(),
                kind: symbol.kind,
                range: symbol.range,
                selection_range: Some(symbol.selection_range),
                detail: symbol.detail.clone(),
                container_name: None,
            },
            children: symbol.children.as_deref().map(nodes).unwrap_or_default(),
        })
        .collect()
}

/// The innermost node whose range contains `position`, with its subtree.
pub fn subtree_at(result: &Value, position: Position) -> Result<Option<Node>> {
    fn descend(nodes: &[Node], position: Position) -> Option<Node> {
        let node = nodes
            .iter()
            .find(|node| contains(node.extent.range, position))?;
        Some(descend(&node.children, position).unwrap_or_else(|| node.clone()))
    }
    Ok(descend(&tree(result)?, position))
}

/// The chain of symbols containing `position`, outermost first, or empty
/// when none does. For the hierarchical shape this is the nesting path;
/// for the flat shape it is every containing symbol from largest to
/// smallest range. `Err` only for a result that is neither shape.
pub fn chain_at(result: &Value, position: Position) -> Result<Vec<Extent>> {
    if result.is_null() {
        return Ok(Vec::new());
    }
    let Some(items) = result.as_array() else {
        anyhow::bail!("documentSymbol result is neither null nor an array: {result}");
    };
    if items.is_empty() {
        return Ok(Vec::new());
    }
    if items[0].get("range").is_some() {
        let symbols: Vec<DocumentSymbol> =
            serde_json::from_value(result.clone()).context("parsing DocumentSymbol[]")?;
        let mut chain = Vec::new();
        let mut level = symbols.as_slice();
        while let Some(symbol) = level.iter().find(|symbol| contains(symbol.range, position)) {
            chain.push(Extent {
                name: symbol.name.clone(),
                kind: symbol.kind,
                range: symbol.range,
                selection_range: Some(symbol.selection_range),
                detail: symbol.detail.clone(),
                container_name: None,
            });
            level = symbol.children.as_deref().unwrap_or(&[]);
        }
        Ok(chain)
    } else {
        let symbols: Vec<SymbolInformation> =
            serde_json::from_value(result.clone()).context("parsing SymbolInformation[]")?;
        let mut containing: Vec<&SymbolInformation> = symbols
            .iter()
            .filter(|symbol| contains(symbol.location.range, position))
            .collect();
        containing.sort_by_key(|symbol| std::cmp::Reverse(span(symbol.location.range)));
        Ok(containing
            .into_iter()
            .map(|symbol| Extent {
                name: symbol.name.clone(),
                kind: symbol.kind,
                range: symbol.location.range,
                selection_range: None,
                detail: None,
                container_name: None,
            })
            .collect())
    }
}

fn span(range: Range) -> (u32, u32) {
    (
        range.end.line - range.start.line,
        range.end.character.wrapping_sub(range.start.character),
    )
}

fn contains(range: Range, position: Position) -> bool {
    let after_start =
        (position.line, position.character) >= (range.start.line, range.start.character);
    let before_end = (position.line, position.character) <= (range.end.line, range.end.character);
    after_start && before_end
}

fn innermost_hierarchical(symbols: &[DocumentSymbol], position: Position) -> Option<Extent> {
    let symbol = symbols
        .iter()
        .find(|symbol| contains(symbol.range, position))?;
    let deeper = symbol
        .children
        .as_deref()
        .and_then(|children| innermost_hierarchical(children, position));
    Some(deeper.unwrap_or_else(|| Extent {
        name: symbol.name.clone(),
        kind: symbol.kind,
        range: symbol.range,
        selection_range: Some(symbol.selection_range),
        detail: symbol.detail.clone(),
        container_name: None,
    }))
}

fn innermost_flat(symbols: &[SymbolInformation], position: Position) -> Option<Extent> {
    symbols
        .iter()
        .filter(|symbol| contains(symbol.location.range, position))
        .min_by_key(|symbol| span(symbol.location.range))
        .map(|symbol| Extent {
            name: symbol.name.clone(),
            kind: symbol.kind,
            range: symbol.location.range,
            selection_range: None,
            detail: None,
            container_name: symbol.container_name.clone(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn range(sl: u32, sc: u32, el: u32, ec: u32) -> Value {
        json!({ "start": { "line": sl, "character": sc }, "end": { "line": el, "character": ec } })
    }

    #[test]
    fn hierarchical_picks_the_innermost_containing_symbol() {
        let outline = json!([
            {
                "name": "Config", "kind": 23,
                "range": range(3, 0, 20, 1), "selectionRange": range(4, 11, 4, 17),
                "children": [
                    { "name": "limit", "kind": 8, "range": range(6, 4, 6, 20), "selectionRange": range(6, 8, 6, 13) },
                    { "name": "new", "kind": 6, "range": range(9, 4, 12, 5), "selectionRange": range(10, 11, 10, 14) }
                ]
            },
            { "name": "choose", "kind": 12, "range": range(22, 0, 25, 1), "selectionRange": range(23, 7, 23, 13) }
        ]);
        let inside_new = innermost_at(&outline, Position::new(10, 11))
            .unwrap()
            .unwrap();
        assert_eq!(inside_new.name, "new");
        assert_eq!(inside_new.kind, SymbolKind::METHOD);
        assert_eq!(inside_new.range.start.line, 9);
        // In the struct but in no child: the struct.
        let on_struct = innermost_at(&outline, Position::new(4, 11))
            .unwrap()
            .unwrap();
        assert_eq!(on_struct.name, "Config");
        // Range ends are inclusive.
        let at_end = innermost_at(&outline, Position::new(25, 1))
            .unwrap()
            .unwrap();
        assert_eq!(at_end.name, "choose");
        // Between items: nothing.
        assert!(
            innermost_at(&outline, Position::new(21, 0))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn chain_is_outermost_first_and_empty_between_items() {
        let outline = json!([
            {
                "name": "tests", "kind": 2,
                "range": range(30, 0, 40, 1), "selectionRange": range(30, 4, 30, 9),
                "children": [
                    { "name": "smoke", "kind": 12, "range": range(32, 4, 35, 5), "selectionRange": range(33, 7, 33, 12) }
                ]
            }
        ]);
        let names: Vec<String> = chain_at(&outline, Position::new(34, 8))
            .unwrap()
            .into_iter()
            .map(|extent| extent.name)
            .collect();
        assert_eq!(names, ["tests", "smoke"]);
        assert!(chain_at(&outline, Position::new(10, 0)).unwrap().is_empty());
        assert!(
            chain_at(&Value::Null, Position::new(0, 0))
                .unwrap()
                .is_empty()
        );

        let uri = "file:///ws/a.py";
        let flat = json!([
            { "name": "doubled", "kind": 6, "location": { "uri": uri, "range": range(9, 4, 12, 5) } },
            { "name": "Config", "kind": 5, "location": { "uri": uri, "range": range(3, 0, 20, 1) } }
        ]);
        let names: Vec<String> = chain_at(&flat, Position::new(10, 0))
            .unwrap()
            .into_iter()
            .map(|extent| extent.name)
            .collect();
        assert_eq!(names, ["Config", "doubled"], "largest range first");
    }

    #[test]
    fn the_tree_nests_children_and_subtree_at_is_the_innermost_node() {
        let outline = json!([
            {
                "name": "Config", "kind": 23,
                "range": range(3, 0, 20, 1), "selectionRange": range(4, 11, 4, 17),
                "children": [
                    { "name": "limit", "kind": 8, "range": range(6, 4, 6, 20), "selectionRange": range(6, 8, 6, 13) }
                ]
            },
            { "name": "choose", "kind": 12, "range": range(22, 0, 25, 1), "selectionRange": range(23, 7, 23, 13) }
        ]);
        let tree = tree(&outline).unwrap();
        assert_eq!(tree.len(), 2);
        assert_eq!(tree[0].children[0].extent.name, "limit");
        let sub = subtree_at(&outline, Position::new(4, 11)).unwrap().unwrap();
        assert_eq!(sub.extent.name, "Config");
        assert_eq!(sub.children.len(), 1);
        assert!(
            subtree_at(&outline, Position::new(21, 0))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn flat_picks_the_smallest_containing_symbol() {
        let uri = "file:///ws/a.py";
        let outline = json!([
            { "name": "Config", "kind": 5, "location": { "uri": uri, "range": range(3, 0, 20, 1) } },
            { "name": "doubled", "kind": 6, "location": { "uri": uri, "range": range(9, 4, 12, 5) } }
        ]);
        let extent = innermost_at(&outline, Position::new(10, 0))
            .unwrap()
            .unwrap();
        assert_eq!(extent.name, "doubled");
        let extent = innermost_at(&outline, Position::new(4, 0))
            .unwrap()
            .unwrap();
        assert_eq!(extent.name, "Config");
    }

    #[test]
    fn empty_or_null_outlines_are_none_and_garbage_is_an_error() {
        assert!(
            innermost_at(&Value::Null, Position::new(0, 0))
                .unwrap()
                .is_none()
        );
        assert!(
            innermost_at(&json!([]), Position::new(0, 0))
                .unwrap()
                .is_none()
        );
        assert!(innermost_at(&json!({"x": 1}), Position::new(0, 0)).is_err());
    }
}
