//! The container a rust-analyzer outline node contributes.
//!
//! An ordinary symbol's selection range names the symbol itself, so its own
//! name is the container. An `impl` node is the one structural shape this
//! server's outline uses: it is named `impl Config`, `impl Label for Config`
//! or `impl<T> Wrapper<T>`, while its selection range names the type the
//! impl is written for — which is the container `workspace/symbol` reports
//! for the methods under it.
//!
//! The type is **parsed out of the advertised header**, not searched for in
//! it: substring matching would accept the trait name (`impl Label for
//! Config` → `Label`), a generic parameter (`impl<T> Wrapper<T>` → `T`) or a
//! component of a qualified self type (`impl crate::a::Config` → `Config`),
//! each of which is a wrong container and so a wrong qualifier match. A
//! header that does not parse as this crate's Rust grammar describes is
//! rejected rather than approximated — including a selection range shorter
//! than a qualified self type, which is a conservative "no container" and
//! not support for that shape.

use tree_sitter::{Node, Parser};

/// The container `name` contributes when its selection range names
/// `selected`, or `None` when this outline gives no container that can be
/// trusted.
pub(super) fn container(name: &str, selected: &str) -> Option<String> {
    if !is_impl_header(name) {
        return (name == selected).then(|| name.to_string());
    }
    (impl_self_type(name).as_deref() == Some(selected)).then(|| selected.to_string())
}

/// Whether this server names an outline node as an impl header. Only the two
/// spellings rust-analyzer emits are structural; an ordinary name that
/// merely begins with those letters (`implementation`) is not.
fn is_impl_header(name: &str) -> bool {
    name.starts_with("impl ") || name.starts_with("impl<")
}

/// The self type of the impl header `name`, from the grammar.
///
/// The header is synthesized into a complete item (`{name} {}`) and parsed;
/// exactly one named root child, an `impl_item`, with no error or missing
/// node anywhere and an **empty** body is required, so a header that already
/// carries a body or more than one item is not read as a header at all.
fn impl_self_type(name: &str) -> Option<String> {
    let mut parser = Parser::new();
    parser.set_language(&tree_sitter_rust::LANGUAGE.into()).ok()?;
    let source = format!("{name} {{}}");
    let tree = parser.parse(&source, None)?;
    let root = tree.root_node();
    if root.named_child_count() != 1 || problem(root) {
        return None;
    }
    let impl_item = root.named_child(0)?;
    if impl_item.kind() != "impl_item" {
        return None;
    }
    let body = impl_item.child_by_field_name("body")?;
    if body.named_child_count() != 0 {
        return None;
    }
    let self_type = impl_item.child_by_field_name("type")?;
    let text = &source[self_type.byte_range()];
    (!text.is_empty()).then(|| text.to_string())
}

/// Whether any node of this tree is a syntax error or a node the grammar had
/// to insert to keep parsing (a missing node): either makes the parse an
/// unreliable basis for a name.
fn problem(node: Node<'_>) -> bool {
    if node.is_error() || node.is_missing() {
        return true;
    }
    let mut cursor = node.walk();
    node.children(&mut cursor).any(problem)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The four impl shapes measured in rust-analyzer's raw documentSymbol
    /// output, with the selection range each one gave and the container its
    /// symbol index reported for the methods under it.
    #[test]
    fn measured_impl_shapes_extract_the_type_they_are_written_for() {
        for (name, expected) in [
            ("impl Config", "Config"),
            ("impl Label for Config", "Config"),
            ("impl std::fmt::Debug for Config", "Config"),
            ("impl<T> Wrapper<T>", "Wrapper<T>"),
            ("impl crate::config::Config", "crate::config::Config"),
            ("impl<T: Clone> Wrapper<T>", "Wrapper<T>"),
        ] {
            assert_eq!(
                impl_self_type(name).as_deref(),
                Some(expected),
                "{name} names its self type"
            );
            assert_eq!(
                container(name, expected),
                Some(expected.to_string()),
                "the range that names exactly the self type is the container"
            );
        }
    }

    /// Every way a selection range other than the self type is refused: the
    /// trait, a generic parameter, a component of a qualified path, and a
    /// longer name that merely contains the text.
    #[test]
    fn only_the_self_type_is_a_container() {
        for (name, selected) in [
            ("impl Label for Config", "Label"),
            ("impl<T> Wrapper<T>", "T"),
            ("impl crate::config::Config", "Config"),
            ("impl Config", "Other"),
            ("impl Item2", "Item"),
            ("impl OtherItem", "Item"),
        ] {
            assert_eq!(
                container(name, selected),
                None,
                "{name} selecting {selected} is not a container"
            );
        }
    }

    #[test]
    fn an_ordinary_name_is_exact_even_when_it_begins_like_an_impl() {
        assert_eq!(
            container("implementation", "implementation"),
            Some("implementation".to_string()),
            "`implementation` is an ordinary name, not an impl header"
        );
        assert_eq!(container("implementation", "mentation"), None);
        assert_eq!(container("impls", "impls"), Some("impls".to_string()));
        assert_eq!(
            container("Config", "Config"),
            Some("Config".to_string())
        );
        assert_eq!(container("Config", "Other"), None);
    }

    #[test]
    fn a_header_that_is_not_one_impl_header_is_rejected() {
        for name in [
            // Not an impl at all.
            "implementation",
            "impl",
            "implx",
            "fn impl_me",
            // An impl and another item, or an impl carrying its own body.
            "impl Config {} struct Other",
            "impl Config { fn new() {} }",
            "impl Config {\n    fn new() {}\n}",
            // Malformed headers the grammar cannot complete.
            "impl for Config",
            "impl Label for",
            "impl<T Wrapper<T>",
            "impl Config;",
        ] {
            let gate = is_impl_header(name);
            let extracted = impl_self_type(name);
            if gate {
                assert_eq!(extracted, None, "{name} is rejected, not approximated");
            } else {
                assert_eq!(
                    container(name, "Config"),
                    None,
                    "{name} takes the ordinary rule"
                );
            }
        }
    }
}
