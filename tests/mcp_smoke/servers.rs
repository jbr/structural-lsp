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
    copy_tree(fixtures, &root);
    std::fs::canonicalize(root).unwrap()
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            if entry.file_name() == "target" {
                continue;
            }
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

fn show(state: &mut structural::state::State, symbol: &str, path: Option<&str>) -> String {
    structural::tools::Show {
        symbol: symbol.to_string(),
        path: path.map(str::to_string),
    }
    .execute(state, &mcplease::types::RequestContext::default())
    .unwrap_or_else(|error| panic!("show {symbol} {path:?}: {error:#}"))
}

#[test]
#[ignore = "spawns rust-analyzer and pyright; run explicitly with -- --ignored"]
fn both_servers_answer_from_one_root() {
    let root = scratch_root();
    let mut state = structural::state::State::new(root.clone());
    let names: Vec<&str> = state.servers().iter().map(|s| s.name()).collect();
    assert_eq!(
        names,
        ["rust-analyzer", "pyright"],
        "both built-ins started: unavailable = {:?}",
        state
            .unavailable()
            .iter()
            .map(|u| format!("{}: {}", u.name, u.reason))
            .collect::<Vec<_>>()
    );

    // `choose` exists in both fixtures: without a scope both servers are
    // asked, and the merge is honestly ambiguous, naming each server.
    let both = show(&mut state, "choose", None);
    assert!(
        both.contains("Ambiguous: 2 exact matches for 'choose'"),
        "{both}"
    );
    assert!(both.contains("· rust-analyzer"), "{both}");
    assert!(both.contains("· pyright"), "{both}");

    // A directory scope narrows the merged candidates to one server's.
    let rust = show(&mut state, "choose", Some("rust"));
    assert!(rust.contains("Resolved: 'choose'"), "{rust}");
    assert!(rust.contains("· rust-analyzer"), "{rust}");
    assert!(rust.contains("pub fn choose"), "{rust}");
    assert!(rust.contains("match mode"), "{rust}");

    let python = show(&mut state, "choose", Some("python"));
    assert!(python.contains("Resolved: 'choose'"), "{python}");
    assert!(python.contains("· pyright"), "{python}");
    assert!(python.contains("python/helper.py"), "{python}");
    // No grammar for Python in this slice: the server's range, stated.
    assert!(python.contains("no shaping grammar for python"), "{python}");

    // A file scope picks the server for its language outright.
    let by_file = show(&mut state, "make", Some("python/main.py"));
    assert!(by_file.contains("Resolved: 'make'"), "{by_file}");
    assert!(by_file.contains("· pyright"), "{by_file}");

    // A name only one language has resolves without a scope.
    let rust_only = show(&mut state, "Mode", None);
    assert!(rust_only.contains("Resolved: 'Mode'"), "{rust_only}");
    assert!(rust_only.contains("pub enum Mode"), "{rust_only}");
    let python_only = show(&mut state, "DEFAULT_LIMIT", None);
    assert!(
        python_only.contains("Resolved: 'DEFAULT_LIMIT'"),
        "{python_only}"
    );
    assert!(python_only.contains("· pyright"), "{python_only}");

    // The footer names both.
    assert!(rust_only.contains("servers: rust-analyzer"), "{rust_only}");
    assert!(rust_only.contains("pyright"), "{rust_only}");

    drop(state);
    let _ = std::fs::remove_dir_all(&root);
}
