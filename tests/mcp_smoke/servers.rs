//! Two servers from one process: rust-analyzer and pyright, started for one
//! root that holds both fixtures. Needs both on `PATH`; gated like the other
//! real-server tests.

use mcplease::traits::Tool;
use std::path::{Path, PathBuf};

/// A scratch copy of both fixtures under one root. Not `fixtures/` itself:
/// rust-analyzer also looks for a `Cargo.toml` in the parent directories,
/// and this repository's own manifest is right there — it would load
/// structural instead of the fixture crate.
fn scratch_root() -> PathBuf {
    let root = std::env::temp_dir().join(format!("structural-both-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let fixtures = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/fixtures"));
    crate::copy_tree(fixtures, &root);
    std::fs::canonicalize(root).unwrap()
}

fn show(state: &mut structural::session::Session, symbol: &str, path: Option<&str>) -> String {
    structural::tools::Show {
        symbol: Some(symbol.to_string()),
        path: path.map(str::to_string),
        anchor: None,
        focus: None,
        view: None,
    }
    .execute(state, &mcplease::types::RequestContext::default())
    .unwrap_or_else(|error| panic!("show {symbol} {path:?}: {error:#}"))
}

fn uses(state: &mut structural::session::Session, symbol: &str, path: Option<&str>) -> String {
    structural::tools::Uses {
        symbol: Some(symbol.to_string()),
        path: path.map(str::to_string),
        anchor: None,
        focus: None,
    }
    .execute(state, &mcplease::types::RequestContext::default())
    .unwrap_or_else(|error| panic!("uses {symbol} {path:?}: {error:#}"))
}

#[test]
#[ignore = "spawns rust-analyzer and pyright; run explicitly with -- --ignored"]
fn both_servers_answer_from_one_root() {
    let root = scratch_root();
    let mut state = structural::session::Session::new(root.clone());
    // Scoped: the guard borrows the server names, and the calls below take
    // the same workspace lock.
    {
        let workspace = state.workspace();
        let names: Vec<&str> = workspace.servers().iter().map(|s| s.name()).collect();
        assert_eq!(
            names,
            ["rust-analyzer", "pyright"],
            "both built-ins started: unavailable = {:?}",
            workspace
                .unavailable()
                .iter()
                .map(|u| format!("{}: {}", u.name, u.reason))
                .collect::<Vec<_>>()
        );
    }

    // `choose` exists in both fixtures: without a scope both servers are
    // asked, and the merge is honestly ambiguous, naming each server.
    let both = show(&mut state, "choose", None);
    assert!(
        both.starts_with("ambiguous: 2 definitions of 'choose'"),
        "{both}"
    );
    assert!(both.contains("fn choose · rust/src/lib.rs:"), "{both}");
    assert!(both.contains("def choose · python/helper.py:"), "{both}");
    assert!(both.contains("· rust-analyzer"), "{both}");
    assert!(both.contains("· pyright"), "{both}");

    // A directory scope narrows the merged candidates to one server's.
    let rust = show(&mut state, "choose", Some("rust"));
    assert!(rust.starts_with("fn choose · rust/src/lib.rs:"), "{rust}");
    assert!(rust.contains("pub fn choose"), "{rust}");
    assert!(rust.contains("match mode"), "{rust}");

    let python = show(&mut state, "choose", Some("python"));
    assert!(
        python.starts_with("def choose · python/helper.py:"),
        "{python}"
    );
    // The whole function, from pyright's documentSymbol extent.
    assert!(
        python.contains("def choose(flag: bool) -> int:"),
        "{python}"
    );
    assert!(python.contains("return 1 if flag else 2"), "{python}");
    assert!(
        !python.contains("note:"),
        "no note on the normal path: {python}"
    );

    // A file scope picks the server for its language outright.
    let by_file = show(&mut state, "make", Some("python/main.py"));
    assert!(
        by_file.starts_with("def make · python/main.py:"),
        "{by_file}"
    );

    // A name only one language has resolves without a scope.
    let rust_only = show(&mut state, "Mode", None);
    assert!(
        rust_only.starts_with("enum Mode · rust/src/lib.rs:"),
        "{rust_only}"
    );
    assert!(rust_only.contains("pub enum Mode"), "{rust_only}");
    let python_only = show(&mut state, "DEFAULT_LIMIT", None);
    assert!(
        python_only.contains(" DEFAULT_LIMIT · python/helper.py:"),
        "{python_only}"
    );

    // `uses` across the same root: unscoped, `choose` is still two things
    // and is listed, not picked; scoped to python, pyright's references
    // are the import and the call in `make`, each with its line.
    let both_uses = uses(&mut state, "choose", None);
    assert!(
        both_uses.starts_with("ambiguous: 2 definitions of 'choose'"),
        "{both_uses}"
    );
    let python_uses = uses(&mut state, "choose", Some("python"));
    assert!(
        python_uses.starts_with("def choose · python/helper.py:"),
        "{python_uses}"
    );
    assert!(
        python_uses.contains("· 2 references · 1 file\npython/main.py\n"),
        "{python_uses}"
    );
    assert!(
        python_uses
            .contains("  3 · from helper import DEFAULT_LIMIT, Config, Label, choose, identity\n"),
        "the import is a reference outside any item: {python_uses}"
    );
    assert!(
        python_uses.contains(" · make · offset = choose(True)\n"),
        "{python_uses}"
    );
    // pyright's outline nests methods under their class.
    let doubled = uses(&mut state, "doubled", Some("python"));
    assert!(
        doubled.contains(" · make · count = config.doubled()\n"),
        "{doubled}"
    );
    let limit = uses(&mut state, "DEFAULT_LIMIT", None);
    assert!(
        limit.contains(
            " · Config::__init__ · def __init__(self, limit: int = DEFAULT_LIMIT) -> None:\n"
        ),
        "the enclosing method is prefixed by its class: {limit}"
    );

    drop(state);
    let _ = std::fs::remove_dir_all(&root);
}
