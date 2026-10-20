//! Module-path qualifiers (`tools::listing::render`, `pkg.listing.render`)
//! on scratch copies of both fixtures with modules added: rust-analyzer
//! reports no container for a free function, so these resolve from the
//! file's path, the file's outline and the directory (docs/resolution.md).
//! Gated: spawns rust-analyzer and pyright.

use std::path::{Path, PathBuf};

const LISTING: &str = "pub fn render() -> usize {\n    1\n}\n\n#[cfg(test)]\nmod tests {\n    \
                       pub fn render() -> usize {\n        2\n    }\n}\n";

/// Both fixtures under one root, with a module tree in each: a Rust
/// `tools/listing.rs` with a same-named function in its `mod tests`, a
/// `host/mod.rs` with a sibling `listing.rs`, and a Python package.
fn scratch_root() -> PathBuf {
    let root = std::env::temp_dir().join(format!("structural-modpath-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    crate::fixtures_in(&root, &["rust", "python"]);
    let write = |relative: &str, text: &str| {
        let path = root.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    };
    let lib = root.join("rust/src/lib.rs");
    let mut text = std::fs::read_to_string(&lib).unwrap();
    text.push_str("\npub mod host;\npub mod tools;\n");
    std::fs::write(&lib, text).unwrap();
    write("rust/src/tools.rs", "pub mod listing;\n");
    write("rust/src/tools/listing.rs", LISTING);
    write(
        "rust/src/host/mod.rs",
        "pub mod listing;\n\npub fn reduce() -> usize {\n    3\n}\n",
    );
    write(
        "rust/src/host/listing.rs",
        "pub fn render() -> usize {\n    4\n}\n",
    );
    write("python/pkg/__init__.py", "def setup():\n    return 1\n");
    write(
        "python/pkg/listing.py",
        "def render():\n    return 2\n\n\nclass Table:\n    def render(self):\n        return 3\n",
    );
    std::fs::canonicalize(root).unwrap()
}

fn show(state: &structural::session::Session, symbol: &str, path: &str) -> String {
    structural::tools::Definition {
        symbol: Some(symbol.to_string()),
        path: Some(path.to_string()),
        anchor: None,
        focus: None,
        view: None,
        page: None,
    }
    .run(state)
    .unwrap_or_else(|error| panic!("{symbol}: {error:#}"))
}

/// The first line of an answer: the header of a resolution, or what a
/// listing says.
fn head(text: &str) -> &str {
    text.lines().next().unwrap_or_default()
}

#[test]
#[ignore = "spawns rust-analyzer and pyright; run explicitly with -- --ignored"]
fn module_paths_resolve_from_the_file_its_outline_and_its_directory() {
    let root = scratch_root();
    let state = structural::session::Session::new(root.clone());
    let rust = |symbol| show(&state, symbol, "rust");
    let python = |symbol| show(&state, symbol, "python");

    // Directories then the file: one segment or several.
    for symbol in ["tools::listing::render", "src::tools::listing::render"] {
        assert_eq!(
            head(&rust(symbol)),
            "fn render · rust/src/tools/listing.rs:1",
            "{symbol}"
        );
    }
    // Two files end the same way: both listed, and the helper inside
    // `mod tests` is not one of them — the top of a file is closer.
    let both = rust("listing::render");
    assert!(
        both.starts_with("ambiguous: 2 definitions of 'listing::render'\n"),
        "{both}"
    );
    assert!(both.contains("rust/src/tools/listing.rs:1"), "{both}");
    assert!(both.contains("rust/src/host/listing.rs:1"), "{both}");
    // The outline nests the helper in `tests`, which the index does not say.
    assert_eq!(
        head(&rust("listing::tests::render")),
        "fn render · rust/src/tools/listing.rs:7"
    );
    // `mod.rs` is its directory's module.
    assert_eq!(
        head(&rust("host::reduce")),
        "fn reduce · rust/src/host/mod.rs:3"
    );

    // Python: the module is the file, the package its directory, and a
    // method's class follows its file.
    assert_eq!(
        head(&python("pkg.listing.render")),
        "def render · python/pkg/listing.py:1"
    );
    assert_eq!(
        head(&python("listing.Table.render")),
        "def render · python/pkg/listing.py:6"
    );
    assert_eq!(
        head(&python("pkg.setup")),
        "def setup · python/pkg/__init__.py:1"
    );

    // A qualifier that names nothing lists the candidates and says what a
    // qualifier is matched against.
    let keyword = rust("crate::tools::listing::render");
    assert!(
        keyword.starts_with(
            "no 'render' is in a container ending in `crate::tools::listing`; the 'render' \
             candidates:\n"
        ),
        "{keyword}"
    );
    assert!(
        keyword.contains("segments are names, so `crate`, `self` and `super` match nothing"),
        "{keyword}"
    );

    drop(state);
    let _ = std::fs::remove_dir_all(Path::new(&root));
}
