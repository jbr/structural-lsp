//! A function's locals are not outline material (docs/tools.md).
//!
//! pyright lists a function's parameters and local variables as `Variable`
//! (or `Constant`, for an upper-case name) children of the function, and
//! vtsls does the same for a TypeScript function, method or constructor —
//! and for an arrow function bound to a `const`, whose locals sit under a
//! `Variable`. rust-analyzer and gopls list no `let` or `:=` bindings, and
//! rust-analyzer lists a `const` or `static` declared in a function body
//! the same way, as a `Constant` under the `Function`. One rule covers every
//! server measured: a `Variable` or `Constant` whose parent is a function or
//! a binding is declared in a body, and it and everything under it are left
//! out. A nested function, class or struct in a body stays: it is an item.

use crate::lsp::outline::Node;
use lsp_types::SymbolKind;

/// A symbol whose children are declared in a body: a callable, or a
/// binding (a variable holding a function expression or an object).
fn is_body(kind: SymbolKind) -> bool {
    matches!(
        kind,
        SymbolKind::FUNCTION
            | SymbolKind::METHOD
            | SymbolKind::CONSTRUCTOR
            | SymbolKind::VARIABLE
            | SymbolKind::CONSTANT
    )
}

fn is_binding(kind: SymbolKind) -> bool {
    matches!(kind, SymbolKind::VARIABLE | SymbolKind::CONSTANT)
}

/// Remove from `nodes` — the children of a symbol of kind `parent`, or the
/// file's top level for `None` — every local and its subtree, at any depth.
/// Returns how many symbols that removed.
pub(super) fn strip(nodes: &mut Vec<Node>, parent: Option<SymbolKind>) -> usize {
    let mut removed = 0;
    if parent.is_some_and(is_body) {
        nodes.retain(|node| {
            let local = is_binding(node.extent.kind);
            if local {
                removed += size(node);
            }
            !local
        });
    }
    for node in nodes.iter_mut() {
        removed += strip(&mut node.children, Some(node.extent.kind));
    }
    removed
}

/// Remove the locals below `nodes` but keep `nodes` themselves: the
/// children of an outline's own target, which were asked for by name.
pub(super) fn strip_below(nodes: &mut [Node]) -> usize {
    nodes
        .iter_mut()
        .map(|node| strip(&mut node.children, Some(node.extent.kind)))
        .sum()
}

/// A node and everything under it.
pub(super) fn size(node: &Node) -> usize {
    1 + node.children.iter().map(size).sum::<usize>()
}
