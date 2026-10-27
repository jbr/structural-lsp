//! Slice 4 item 1: qualified names and anchors, on scratch copies of both
//! fixtures with a second `new` added. Gated: spawns rust-analyzer and
//! pyright.

use std::path::PathBuf;

/// Both fixtures under one root, plus a second type with a `new` in the
/// Rust one so the bare name is ambiguous.
fn scratch_root() -> PathBuf {
    let root = std::env::temp_dir().join(format!("structural-target-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    crate::fixtures_in(&root, &["rust", "python"]);
    let lib = root.join("rust/src/lib.rs");
    let mut text = std::fs::read_to_string(&lib).unwrap();
    text.push_str("\npub struct Other;\n\nimpl Other {\n    pub fn new() -> Self {\n        Other\n    }\n}\n");
    std::fs::write(&lib, text).unwrap();
    std::fs::canonicalize(root).unwrap()
}

fn show(
    state: &structural_lsp::session::Session,
    symbol: Option<&str>,
    path: Option<&str>,
    anchor: Option<&str>,
    focus: Option<&str>,
) -> anyhow::Result<String> {
    structural_lsp::tools::Definition {
        symbol: symbol.map(str::to_string),
        path: path.map(str::to_string),
        anchor: anchor.map(str::to_string),
        focus: focus.map(str::to_string),
        view: None,
        page: None,
        workspace: None,
        language: None,
    }
    .run(state)
}

fn uses(
    state: &structural_lsp::session::Session,
    symbol: Option<&str>,
    path: Option<&str>,
    anchor: Option<&str>,
    focus: Option<&str>,
) -> anyhow::Result<String> {
    structural_lsp::tools::References {
        symbol: symbol.map(str::to_string),
        path: path.map(str::to_string),
        anchor: anchor.map(str::to_string),
        focus: focus.map(str::to_string),
        within: None,
        page: None,
        workspace: None,
        language: None,
    }
    .run(state)
}

#[test]
#[ignore = "spawns rust-analyzer and pyright; run explicitly with -- --ignored"]
fn qualified_names_and_anchors_target_what_bare_names_cannot() {
    let root = scratch_root();
    let state = structural_lsp::session::Session::new(root.clone());
    let ok = |result: anyhow::Result<String>| result.unwrap_or_else(|error| panic!("{error:#}"));

    // The bare name is two things; each qualifier picks one; a wrong
    // qualifier lists the containers the server reported.
    let bare = ok(show(&state, Some("new"), Some("rust"), None, None));
    assert!(
        bare.starts_with("ambiguous: 2 definitions of 'new'"),
        "{bare}"
    );
    let config_new = ok(show(&state, Some("Config::new"), Some("rust"), None, None));
    assert!(
        config_new.starts_with("fn new · rust/src/lib.rs:23\n"),
        "{config_new}"
    );
    assert!(
        config_new.contains("pub fn new(limit: usize) -> Self {"),
        "{config_new}"
    );
    let other_new = ok(show(&state, Some("Other.new"), Some("rust"), None, None));
    assert!(
        other_new.starts_with("fn new · rust/src/lib.rs:"),
        "{other_new}"
    );
    assert!(
        other_new.contains("pub fn new() -> Self {\n        Other"),
        "{other_new}"
    );
    let wrong = ok(show(&state, Some("Missing::new"), Some("rust"), None, None));
    assert!(
        wrong
            .starts_with("no 'new' is in a container ending in `Missing`; the 'new' candidates:\n"),
        "{wrong}"
    );
    assert!(
        wrong.contains("fn new · rust/src/lib.rs:23 · in Config\n"),
        "{wrong}"
    );
    assert!(wrong.contains(" · in Other\n"), "{wrong}");
    // pyright spells the container the same way.
    let doubled = ok(show(
        &state,
        Some("Config.doubled"),
        Some("python"),
        None,
        None,
    ));
    assert!(
        doubled.starts_with("def doubled · python/helper.py:15\n"),
        "{doubled}"
    );

    // An anchor on a local: its definition is its line, with no kind
    // word, and its uses are the local's, not the struct's.
    let anchor = "let config = Config::new(3);";
    let local = ok(show(
        &state,
        None,
        Some("rust/src/lib.rs"),
        Some(anchor),
        Some("config"),
    ));
    assert_eq!(
        local, "config · rust/src/lib.rs:56\nlet config = Config::new(3);\n",
        "{local}"
    );
    let local_uses = ok(uses(
        &state,
        None,
        Some("rust/src/lib.rs"),
        Some(anchor),
        Some("config"),
    ));
    assert_eq!(
        local_uses,
        "config · rust/src/lib.rs:56 · 2 references · 1 file\nrust/src/lib.rs\n  58 · run · let count = config.doubled();\n  68 · run · let label = config.label();\n",
        "{local_uses}"
    );
    // A focus on a method call resolves to the method's definition.
    let method = ok(show(
        &state,
        None,
        Some("rust/src/lib.rs"),
        Some("let count = config.doubled();"),
        Some("doubled"),
    ));
    assert!(
        method.starts_with("fn doubled · rust/src/lib.rs:28\n"),
        "{method}"
    );
    assert!(
        method.contains("pub fn doubled(&self) -> usize {"),
        "{method}"
    );
    // The whole anchor as the focus starts at its first token.
    let whole = ok(show(
        &state,
        None,
        Some("rust/src/lib.rs"),
        Some("choose(Mode::Fast);"),
        None,
    ));
    assert!(
        whole.starts_with("fn choose · rust/src/lib.rs:43\n"),
        "{whole}"
    );

    // Python: a local in `make`.
    let py_uses = ok(uses(
        &state,
        None,
        Some("python/main.py"),
        Some("config = Config(DEFAULT_LIMIT)"),
        Some("config"),
    ));
    assert!(
        py_uses.starts_with("variable config · python/main.py:9 · 1 reference · 1 file\n"),
        "{py_uses}"
    );
    assert!(
        py_uses.contains("  11 · make · count = config.doubled()\n"),
        "{py_uses}"
    );

    // Errors say what to fix.
    let missing = show(
        &state,
        None,
        Some("rust/src/lib.rs"),
        Some("nothing like this"),
        None,
    )
    .unwrap_err();
    assert!(
        missing.to_string().contains("anchor not found"),
        "{missing:#}"
    );
    let twice = show(
        &state,
        None,
        Some("rust/src/lib.rs"),
        Some("let count ="),
        None,
    )
    .unwrap_err();
    assert!(
        twice
            .to_string()
            .contains("anchor occurs 2 times, at lines 58, 60"),
        "{twice:#}"
    );
    let no_path = show(&state, None, None, Some(anchor), None).unwrap_err();
    assert!(
        no_path.to_string().contains("`anchor` needs `path`"),
        "{no_path:#}"
    );
    let dir = show(&state, None, Some("rust"), Some(anchor), None).unwrap_err();
    assert!(dir.to_string().contains("must name a file"), "{dir:#}");
    let both = show(
        &state,
        Some("choose"),
        Some("rust/src/lib.rs"),
        Some(anchor),
        None,
    )
    .unwrap_err();
    assert!(both.to_string().contains("not both"), "{both:#}");
    let neither = show(&state, None, None, None, None).unwrap_err();
    assert!(
        neither.to_string().contains("give a `symbol`"),
        "{neither:#}"
    );
    let stray_focus = show(&state, Some("choose"), None, None, Some("x")).unwrap_err();
    assert!(
        stray_focus.to_string().contains("`focus` selects within"),
        "{stray_focus:#}"
    );

    drop(state);
    let _ = std::fs::remove_dir_all(&root);
}
