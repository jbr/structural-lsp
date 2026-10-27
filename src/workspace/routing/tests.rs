//! The routing rule with a `language`, and the argument errors it can be —
//! all decided on laid-out slots, with nothing started.

use super::*;
use std::sync::atomic::AtomicBool;

/// A tree with Rust and Python sources and an extensionless script; its
/// slots are `[pyright, rust-analyzer]`, in plan order.
fn two_languages(name: &str) -> Workspace {
    let root =
        std::env::temp_dir().join(format!("structural-routing-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for (path, contents) in [
        ("Cargo.toml", "[package]\nname = \"x\"\n"),
        ("src/lib.rs", "pub fn choose() {}\n"),
        ("app.py", "def choose():\n    pass\n"),
        ("bin/tool", "#!/bin/sh\n"),
    ] {
        let file = root.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(file, contents).unwrap();
    }
    let workspace = Workspace::new(
        root,
        &crate::config::from_file(),
        Arc::new(AtomicBool::new(false)),
        Arc::new(crate::clock::SystemClock),
        Arc::new(crate::host::observation::Pending::new()),
    );
    let names: Vec<&str> = workspace.slots().iter().map(|slot| slot.name()).collect();
    assert_eq!(names, ["pyright", "rust-analyzer"]);
    workspace
}

#[test]
fn a_language_narrows_every_scope_through_the_one_rule() {
    let workspace = two_languages("narrows");
    let root = workspace.root().to_path_buf();
    let slots = workspace.slots();
    let route = |scope: Option<&Path>, language: Option<&str>| serving(slots, scope, language);

    // No scope and a directory scope: every slot, then the language's.
    assert_eq!(route(None, None), [0, 1]);
    assert_eq!(route(None, Some("rust")), [1]);
    assert_eq!(route(None, Some("Python")), [0]);
    assert_eq!(route(None, Some("rs")), [1]);
    assert_eq!(route(None, Some(".py")), [0]);
    assert_eq!(route(Some(&root.join("src")), Some("python")), [0]);
    assert!(route(None, Some("go")).is_empty());

    // A file is routed by its language first; a language that agrees
    // changes nothing, and one that contradicts it leaves nothing.
    let py = root.join("app.py");
    assert_eq!(route(Some(&py), None), [0]);
    assert_eq!(route(Some(&py), Some("python")), [0]);
    assert!(route(Some(&py), Some("rust")).is_empty());

    // A file with no extension routes to every slot, so a language is
    // what routes it.
    let script = root.join("bin/tool");
    assert_eq!(route(Some(&script), None), [0, 1]);
    assert_eq!(route(Some(&script), Some("rust")), [1]);
}

#[test]
fn a_language_no_server_here_speaks_is_an_argument_error_listing_the_languages() {
    let workspace = two_languages("unknown");
    let error = workspace
        .check_language(None, Some("go"))
        .unwrap_err()
        .to_string();
    assert!(
        error.starts_with("`language: go` matches no language server here"),
        "{error}"
    );
    assert!(
        error.contains("languages here: python (.py, .pyi), rust (.rs)"),
        "{error}"
    );
    // Why there is no Go server: the tree does not call for one.
    assert!(error.contains("not here: gopls"), "{error}");
    // `select` says the same, so no path can skip the check.
    let selected = workspace.select(None, Some("go")).unwrap_err().to_string();
    assert_eq!(selected, error);
}

#[test]
fn a_file_path_that_contradicts_the_language_says_so() {
    let workspace = two_languages("contradicts");
    let py = workspace.root().join("app.py");
    let error = workspace
        .check_language(Some(&py), Some("rust"))
        .unwrap_err()
        .to_string();
    assert!(
        error.starts_with("`path` is a `py` file, which `language: rust` does not cover"),
        "{error}"
    );
    assert!(error.contains("served by pyright"), "{error}");
    // Agreeing, or naming a directory or an extensionless file, is valid.
    workspace.check_language(Some(&py), Some("py")).unwrap();
    workspace
        .check_language(Some(&workspace.root().join("src")), Some("rust"))
        .unwrap();
    workspace
        .check_language(Some(&workspace.root().join("bin/tool")), Some("python"))
        .unwrap();
    workspace.check_language(Some(&py), None).unwrap();
}

#[test]
fn a_valid_language_does_not_depend_on_what_is_running() {
    // Nothing is running here. The argument is valid already, and the
    // query's error names only the server the language routes to — the
    // call stays the same call once that server is up.
    let workspace = two_languages("running");
    workspace.check_language(None, Some("rust")).unwrap();
    let error = workspace
        .select(None, Some("rust"))
        .unwrap_err()
        .to_string();
    assert!(error.contains("rust-analyzer: not started"), "{error}");
    assert!(!error.contains("pyright"), "{error}");
}

#[test]
fn a_tree_that_calls_for_no_server_says_so_rather_than_naming_an_empty_extension() {
    let root =
        std::env::temp_dir().join(format!("structural-routing-{}-empty", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let workspace = Workspace::new(
        root.clone(),
        &crate::config::from_file(),
        Arc::new(AtomicBool::new(false)),
        Arc::new(crate::clock::SystemClock),
        Arc::new(crate::host::observation::Pending::new()),
    );
    assert!(workspace.slots().is_empty());
    let error = workspace.select(None, None).unwrap_err().to_string();
    assert!(
        error.starts_with(
            "no language server is set up here: nothing in this tree calls for one.\n"
        ),
        "{error}"
    );
    assert!(
        !error.contains("``") && !error.contains("here: \n"),
        "{error}"
    );
    // Why each catalogued server is absent still follows.
    assert!(error.contains("not here: "), "{error}");
    let _ = std::fs::remove_dir_all(&root);
}
