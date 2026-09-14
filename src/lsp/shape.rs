//! Tree-sitter shaping: decorating an item's extent, and finding the Rust
//! item containing a position when the server gives no extent.
//!
//! This is the syntax half of the design's "LSP resolves, tree-sitter
//! shapes". The item's *extent* normally comes from the server's
//! `documentSymbol` ([`crate::lsp::outline`]); [`extend_start`] then widens
//! its start backwards over the leading doc comments and outer attributes
//! (Rust) or decorators (Python) that belong to it — the same decorated-unit
//! rule as semantic-edit-mcp's `item.rs`, borrowed as an approach, not
//! extracted as a shared crate. A language without a grammar keeps the
//! server's extent. [`item_at`] is the fallback when a Rust server gives no
//! extent at all.
//!
//! Selection semantics: the *smallest* enclosing named item wins. A position
//! inside a method's signature or body yields the method, not its `impl`
//! block; a position on a field yields the struct/enum. When no item
//! contains the position (blank line, file start), `None` is returned and
//! callers must fall back to the server's reported range explicitly rather
//! than guessing an item.
//!
//! Line-ending note: `Point` rows/columns are byte-based and derived with a
//! `\n`-counting helper; CRLF files give correct results because the `\r`
//! ends up in the column count, bare-CR files are outside the supported
//! subset (see `positions.rs` for the LSP-side contract).

use anyhow::{Context, Result};
use std::ops::Range;
use tree_sitter::{Node, Parser, Point};

/// The Rust grammar's item-shaped nodes, mapped to display kinds.
const ITEM_KINDS: &[(&str, &str)] = &[
    ("function_item", "fn"),
    ("struct_item", "struct"),
    ("enum_item", "enum"),
    ("union_item", "union"),
    ("trait_item", "trait"),
    ("impl_item", "impl"),
    ("mod_item", "mod"),
    ("type_item", "type"),
    ("const_item", "const"),
    ("static_item", "static"),
    ("macro_definition", "macro"),
    ("foreign_mod_item", "extern"),
];

fn display_kind(kind: &str) -> Option<&'static str> {
    ITEM_KINDS
        .iter()
        .find(|(node_kind, _)| *node_kind == kind)
        .map(|(_, display)| *display)
}

/// The item found at a position.
#[derive(Debug, Clone)]
pub struct Item {
    /// The grammar's own node kind (e.g. `struct_item`). Owned: the node
    /// borrows the parse tree, which does not outlive this function. Read by
    /// the shaping tests; `display_kind` carries the same fact for the tool.
    pub kind: String,
    /// Display kind (e.g. `struct`).
    pub display_kind: &'static str,
    /// The item's declared name, when the grammar gives it one. `impl` blocks
    /// have none; callers show their header instead.
    pub name: Option<String>,
    /// The item's own span, without decorations.
    pub range: Range<usize>,
    /// The span including the leading doc comments and outer attributes.
    pub decorated_range: Range<usize>,
}

impl Item {
    /// The decorated source slice, budgeted by the caller.
    pub fn decorated_text<'a>(&self, source: &'a str) -> &'a str {
        &source[self.decorated_range.clone()]
    }

    /// The item's declaration line (first line of the undecorated span),
    /// trimmed — useful for `impl` blocks and as an identity fallback.
    pub fn header(&self, source: &str) -> String {
        let text = &source[self.range.clone()];
        text.lines().next().unwrap_or(text).trim().to_string()
    }
}

/// A grammar for a language id, when this crate carries one.
fn grammar(language: &str) -> Option<tree_sitter::Language> {
    match language {
        "rust" => Some(tree_sitter_rust::LANGUAGE.into()),
        "python" => Some(tree_sitter_python::LANGUAGE.into()),
        _ => None,
    }
}

/// Widen an item's start backwards over its leading decorations: outer doc
/// comments and outer attributes in Rust, decorators in Python. `start` is
/// the byte offset where the server says the item begins; the result is the
/// same offset when nothing decorates it, when the server already included
/// the decorations, or when the language has no grammar here.
pub fn extend_start(language: &str, source: &str, start: usize) -> Result<usize> {
    let Some(grammar) = grammar(language) else {
        return Ok(start);
    };
    if start > source.len() || !source.is_char_boundary(start) {
        anyhow::bail!(
            "shaping offset {start} is not inside the {len}-byte source",
            len = source.len()
        );
    }
    let mut parser = Parser::new();
    parser
        .set_language(&grammar)
        .with_context(|| format!("loading the {language} tree-sitter grammar"))?;
    let tree = parser
        .parse(source, None)
        .context("parsing for shaping failed")?;
    let Some(mut node) = tree.root_node().descendant_for_byte_range(start, start) else {
        return Ok(start);
    };
    // The outermost node that begins exactly here: from the first token up
    // to the item it starts.
    while let Some(parent) = node.parent() {
        if parent.start_byte() != start {
            break;
        }
        node = parent;
    }
    Ok(decoration_start(language, source, node))
}

/// Find the smallest decorated item containing `byte_offset`.
///
/// Returns `Ok(None)` when no item contains the position — a real answer,
/// not an error; callers fall back to the server's range.
pub fn item_at(source: &str, byte_offset: usize) -> Result<Option<Item>> {
    if byte_offset > source.len() || !source.is_char_boundary(byte_offset) {
        anyhow::bail!(
            "shaping offset {byte_offset} is not inside the {len}-byte source",
            len = source.len()
        );
    }
    let mut parser = Parser::new();
    parser
        .set_language(&tree_sitter_rust::LANGUAGE.into())
        .context("loading the Rust tree-sitter grammar")?;
    let tree = parser
        .parse(source, None)
        .context("parsing for shaping failed")?;

    let point = byte_point(source, byte_offset);
    let Some(mut node) = tree.root_node().descendant_for_point_range(point, point) else {
        return Ok(None);
    };
    loop {
        if display_kind(node.kind()).is_some() {
            break;
        }
        match node.parent() {
            Some(parent) => node = parent,
            None => return Ok(None),
        }
    }

    let display = display_kind(node.kind()).expect("checked above");
    let kind = node.kind().to_string();
    let name = node
        .child_by_field_name("name")
        .map(|name_node| source[name_node.start_byte()..name_node.end_byte()].to_string());
    let decorated_start = decoration_start("rust", source, node);
    Ok(Some(Item {
        kind,
        display_kind: display,
        name,
        range: node.start_byte()..node.end_byte(),
        decorated_range: decorated_start..node.end_byte(),
    }))
}

/// Whether a sibling is part of the item's decoration. Rust: an outer
/// attribute, or an *outer* doc comment (`///`, `/**`) — inner docs (`//!`,
/// `/*!`) belong to the enclosing item and plain comments are not
/// decoration, so both stop the run. Python: a decorator (the docstring is
/// inside the body already).
fn is_outer_decoration(language: &str, kind: &str, text: &str) -> bool {
    match (language, kind) {
        ("rust", "line_comment") => text.starts_with("///") && !text.starts_with("////"),
        ("rust", "block_comment") => text.starts_with("/**") && !text.starts_with("/**!"),
        ("rust", "attribute_item") => true,
        ("python", "decorator") => true,
        _ => false,
    }
}

/// Walk backwards over the contiguous run of decorations that belong to this
/// item, returning the decorated start.
fn decoration_start(language: &str, source: &str, node: Node<'_>) -> usize {
    let mut start = node.start_byte();
    let mut sibling = node.prev_sibling();
    while let Some(prev) = sibling {
        if is_outer_decoration(
            language,
            prev.kind(),
            &source[prev.start_byte()..prev.end_byte()],
        ) {
            start = prev.start_byte();
            sibling = prev.prev_sibling();
        } else {
            break;
        }
    }
    // Trim back to the first character of the run so byte columns align.
    while start > 0 && !source.is_char_boundary(start) {
        start -= 1;
    }
    while start < source.len() && source[start..].starts_with(['\n', '\r']) {
        start += 1;
    }
    start
}

/// Byte offset to a tree-sitter `Point` (row and *byte* column).
fn byte_point(source: &str, offset: usize) -> Point {
    let bytes = &source.as_bytes()[..offset];
    let row = bytes.iter().filter(|b| **b == b'\n').count();
    let line_start = bytes
        .iter()
        .rposition(|b| *b == b'\n')
        .map(|index| index + 1)
        .unwrap_or(0);
    Point {
        row,
        column: offset - line_start,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = r#"//! Crate doc.
//! More crate doc.

/// Settings for a session.
#[derive(Debug, Clone)]
pub struct Config {
    /// The limit.
    pub limit: usize,
}

impl Config {
    /// Double the limit.
    pub fn doubled(&self) -> usize {
        self.limit * 2
    }
}
"#;

    fn offset_of(needle: &str) -> usize {
        SOURCE.find(needle).expect("needle present")
    }

    #[test]
    fn struct_item_carries_its_doc_and_attributes() {
        let item = item_at(SOURCE, offset_of("pub struct Config"))
            .unwrap()
            .unwrap();
        assert_eq!(item.kind, "struct_item");
        assert_eq!(item.display_kind, "struct");
        assert_eq!(item.name.as_deref(), Some("Config"));
        let decorated = item.decorated_text(SOURCE);
        assert!(
            decorated.starts_with("/// Settings for a session."),
            "{decorated}"
        );
        assert!(decorated.contains("#[derive(Debug, Clone)]"));
        // The field inside the struct resolves to the struct — no item
        // smaller than the struct contains it.
        let field = item_at(SOURCE, offset_of("pub limit")).unwrap().unwrap();
        assert_eq!(field.name.as_deref(), Some("Config"));
    }

    #[test]
    fn method_resolves_to_the_fn_not_the_impl() {
        let item = item_at(SOURCE, offset_of("pub fn doubled"))
            .unwrap()
            .unwrap();
        assert_eq!(item.kind, "function_item");
        assert_eq!(item.name.as_deref(), Some("doubled"));
        let decorated = item.decorated_text(SOURCE);
        assert!(
            decorated.starts_with("/// Double the limit."),
            "{decorated}"
        );
        assert!(!decorated.contains("impl Config"));
    }

    #[test]
    fn selection_inside_a_signature_resolves_to_the_same_fn() {
        let at_name = item_at(SOURCE, offset_of("pub fn doubled"))
            .unwrap()
            .unwrap();
        let at_params = item_at(SOURCE, offset_of("&self")).unwrap().unwrap();
        assert_eq!(at_name.range, at_params.range);
        // Inside the body too.
        let at_body = item_at(SOURCE, offset_of("self.limit * 2"))
            .unwrap()
            .unwrap();
        assert_eq!(at_name.range, at_body.range);
    }

    #[test]
    fn impl_keyword_resolves_to_the_impl_block() {
        let item = item_at(SOURCE, offset_of("impl Config")).unwrap().unwrap();
        assert_eq!(item.kind, "impl_item");
        assert_eq!(item.name, None);
        assert!(item.header(SOURCE).starts_with("impl Config"));
    }

    #[test]
    fn position_outside_any_item_is_none() {
        // On the crate doc, above every item.
        assert!(item_at(SOURCE, 0).unwrap().is_none());
        // Past the end of the last item (the trailing newline).
        assert!(item_at(SOURCE, SOURCE.len() - 1).unwrap().is_none());
    }

    #[test]
    fn inner_doc_comments_of_an_enclosing_item_stay_enclosed() {
        let source = "mod inner {\n    //! Inner doc.\n    pub fn f() {}\n}\n";
        let item = item_at(source, source.find("fn f").unwrap())
            .unwrap()
            .unwrap();
        assert_eq!(item.kind, "function_item");
        assert_eq!(item.name.as_deref(), Some("f"));
        // The inner doc belongs to the mod, not the fn.
        assert!(!item.decorated_text(source).contains("Inner doc."));
        let module = item_at(source, source.find("mod inner").unwrap())
            .unwrap()
            .unwrap();
        assert!(module.decorated_text(source).contains("//! Inner doc."));
    }

    #[test]
    fn extend_start_widens_over_rust_docs_and_attributes() {
        let start = SOURCE.find("pub struct Config").unwrap();
        let extended = extend_start("rust", SOURCE, start).unwrap();
        assert!(
            SOURCE[extended..].starts_with("/// Settings for a session."),
            "{}",
            &SOURCE[extended..extended + 40]
        );
        // Already at the decoration: unchanged.
        assert_eq!(extend_start("rust", SOURCE, extended).unwrap(), extended);
        // A documented method: widened to its doc comment, which then runs
        // straight into the signature.
        let doubled = SOURCE.find("pub fn doubled").unwrap();
        let extended = extend_start("rust", SOURCE, doubled).unwrap();
        assert!(extended < doubled, "widened");
        assert!(
            SOURCE[extended..].starts_with("///"),
            "{}",
            &SOURCE[extended..doubled]
        );
        assert!(
            !SOURCE[extended..doubled].contains("\n\n"),
            "contiguous decoration"
        );
    }

    #[test]
    fn extend_start_widens_over_python_decorators_and_leaves_other_languages_alone() {
        let source =
            "import functools\n\n@functools.cache\n@other\ndef choose(flag):\n    return 1\n";
        let start = source.find("def choose").unwrap();
        let extended = extend_start("python", source, start).unwrap();
        assert!(
            source[extended..].starts_with("@functools.cache"),
            "{}",
            &source[extended..]
        );
        // Starting at the first decorator already: unchanged.
        assert_eq!(extend_start("python", source, extended).unwrap(), extended);
        // No grammar: the server's start stands.
        assert_eq!(extend_start("go", "func Greet() {}", 0).unwrap(), 0);
    }
}
