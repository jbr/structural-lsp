//! Which hits are tests, and how an enclosing item is named, for every
//! location listing (`uses`, the location views). Both are heuristics
//! over the outline chain and the path; the result says so.

use crate::lsp::outline::Extent;
use lsp_types::SymbolKind;
use std::path::Path;

/// A named container worth prefixing an item with: a module, namespace,
/// class, struct, enum or interface. Impl blocks and functions are not.
fn is_container(kind: SymbolKind) -> bool {
    matches!(
        kind,
        SymbolKind::MODULE
            | SymbolKind::NAMESPACE
            | SymbolKind::PACKAGE
            | SymbolKind::CLASS
            | SymbolKind::STRUCT
            | SymbolKind::ENUM
            | SymbolKind::INTERFACE
    )
}

/// A symbol that holds a value rather than code. pyright lists parameters
/// and local variables as children of a function, and a hit "in" one of
/// those is really in the function; a top-level `const` or `static`
/// (rust-analyzer reports those as constant/variable too) is the item.
fn is_value(kind: SymbolKind) -> bool {
    matches!(
        kind,
        SymbolKind::VARIABLE
            | SymbolKind::CONSTANT
            | SymbolKind::FIELD
            | SymbolKind::PROPERTY
            | SymbolKind::ENUM_MEMBER
            | SymbolKind::TYPE_PARAMETER
            | SymbolKind::STRING
            | SymbolKind::NUMBER
            | SymbolKind::BOOLEAN
            | SymbolKind::ARRAY
            | SymbolKind::KEY
            | SymbolKind::NULL
    )
}

fn is_callable(kind: SymbolKind) -> bool {
    matches!(
        kind,
        SymbolKind::FUNCTION | SymbolKind::METHOD | SymbolKind::CONSTRUCTOR
    )
}

/// The innermost item's name, prefixed by its named containers joined with
/// `::` (`tests::smoke`, `Config::doubled`). A value symbol nested directly
/// in a function or method (a parameter, a local) is not the item, its
/// function is; a value anywhere else (a top-level constant, a class
/// attribute) is. `None` outside every item.
pub fn item_label(chain: &[Extent]) -> Option<String> {
    let innermost = chain
        .iter()
        .enumerate()
        .rev()
        .find(|(index, extent)| {
            let parent = index.checked_sub(1).map(|parent| chain[parent].kind);
            !(is_value(extent.kind) && parent.is_some_and(is_callable))
        })
        .map(|(index, _)| index)?;
    let (last, containers) = chain[..=innermost].split_last()?;
    let mut label = String::new();
    for container in containers.iter().filter(|extent| is_container(extent.kind)) {
        label.push_str(&container.name);
        label.push_str("::");
    }
    label.push_str(&last.name);
    Some(label)
}

/// A hit inside a module named `tests` or `test` is a test.
pub fn chain_is_test(chain: &[Extent]) -> bool {
    chain.iter().any(|extent| {
        matches!(extent.kind, SymbolKind::MODULE | SymbolKind::NAMESPACE)
            && matches!(extent.name.as_str(), "tests" | "test")
    })
}

/// A file under a `tests`/`test` directory, or named `tests.*`, `test_*`,
/// `*_test.*` or `*.test.*`, is a test file (`foo.rs` + `foo/tests.rs` is
/// the sibling-module convention).
pub fn path_is_test(path: &Path) -> bool {
    let in_test_dir = path
        .parent()
        .into_iter()
        .flat_map(Path::components)
        .any(|component| matches!(component.as_os_str().to_str(), Some("tests" | "test")));
    let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
        return in_test_dir;
    };
    let stem = file_name
        .split_once('.')
        .map_or(file_name, |(stem, _)| stem);
    in_test_dir
        || matches!(stem, "tests" | "test")
        || stem.starts_with("test_")
        || stem.ends_with("_test")
        || file_name.contains(".test.")
}

#[cfg(test)]
mod tests {
    use super::*;
    use lsp_types::{Position, Range};

    fn extent(name: &str, kind: SymbolKind) -> Extent {
        Extent {
            name: name.to_string(),
            kind,
            range: Range::new(Position::new(0, 0), Position::new(0, 0)),
            selection_range: None,
            detail: None,
        }
    }

    #[test]
    fn labels_skip_impls_and_keep_named_containers() {
        let chain = [
            extent("impl Config", SymbolKind::OBJECT),
            extent("new", SymbolKind::METHOD),
        ];
        assert_eq!(item_label(&chain).as_deref(), Some("new"));
        let chain = [
            extent("tests", SymbolKind::MODULE),
            extent("smoke", SymbolKind::FUNCTION),
        ];
        assert_eq!(item_label(&chain).as_deref(), Some("tests::smoke"));
        let chain = [
            extent("Config", SymbolKind::CLASS),
            extent("doubled", SymbolKind::METHOD),
        ];
        assert_eq!(item_label(&chain).as_deref(), Some("Config::doubled"));
        assert_eq!(item_label(&[]), None);
        // pyright nests a parameter under its method; the method is the item.
        let chain = [
            extent("Config", SymbolKind::CLASS),
            extent("__init__", SymbolKind::CONSTRUCTOR),
            extent("limit", SymbolKind::VARIABLE),
        ];
        assert_eq!(item_label(&chain).as_deref(), Some("Config::__init__"));
        // A nested local is skipped all the way to the function.
        let chain = [
            extent("make", SymbolKind::FUNCTION),
            extent("config", SymbolKind::VARIABLE),
        ];
        assert_eq!(item_label(&chain).as_deref(), Some("make"));
        // A top-level constant, or one in a module, is the item: its kind
        // is the same but its parent is not a function.
        let chain = [extent("DEFAULT_LIMIT", SymbolKind::CONSTANT)];
        assert_eq!(item_label(&chain).as_deref(), Some("DEFAULT_LIMIT"));
        let chain = [
            extent("limits", SymbolKind::MODULE),
            extent("LIMIT", SymbolKind::CONSTANT),
        ];
        assert_eq!(item_label(&chain).as_deref(), Some("limits::LIMIT"));
        let chain = [
            extent("Config", SymbolKind::CLASS),
            extent("limit", SymbolKind::FIELD),
        ];
        assert_eq!(item_label(&chain).as_deref(), Some("Config::limit"));
    }

    #[test]
    fn tests_are_a_module_name_or_a_path_shape() {
        assert!(chain_is_test(&[
            extent("tests", SymbolKind::MODULE),
            extent("smoke", SymbolKind::FUNCTION)
        ]));
        assert!(!chain_is_test(&[extent("tests", SymbolKind::FUNCTION)]));
        assert!(path_is_test(Path::new("/r/tests/mcp_smoke/main.rs")));
        assert!(path_is_test(Path::new("/r/pkg/test_helper.py")));
        assert!(path_is_test(Path::new("/r/pkg/greet_test.go")));
        assert!(path_is_test(Path::new("/r/src/a.test.ts")));
        assert!(path_is_test(Path::new("/r/src/definition/tests.rs")));
        assert!(!path_is_test(Path::new("/r/src/testsuite.rs")));
        assert!(!path_is_test(Path::new("/r/src/latest_tester.rs")));
    }
}
