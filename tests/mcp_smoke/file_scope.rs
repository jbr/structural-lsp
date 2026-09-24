//! A `path` that names a file nominates from that file's own outline, so a
//! common name resolves from the file that declares it even when the
//! servers' `workspace/symbol` pool does not reach it. What makes that pool
//! inadequate is a *property of the file*, not a number this test asserts:
//! `src/models.rs` below declares more `new`s than rust-analyzer's
//! `workspace/symbol` response returns at all (128 on the version this was
//! written against), so the last of them never enter the index pool — the
//! index's own truncation is recorded in the journal, not required here.
//!
//! Gated: spawns rust-analyzer and pyright.

use std::path::PathBuf;

/// Types in `src/models.rs`: enough same-named definitions in one file that
/// a bounded symbol-index response cannot hold them all.
const MODELS: usize = 140;
/// Modules of other types, so the workspace is not one file.
const MODULES: usize = 4;
const MODULE_TYPES: usize = 12;

fn model(index: usize) -> String {
    format!(
        "pub struct Model{index} {{\n    text: String,\n}}\n\n\
         impl Model{index} {{\n    \
         pub fn new(text: String) -> Self {{\n        Self {{ text }}\n    }}\n\n    \
         pub fn text(&self) -> &str {{\n        &self.text\n    }}\n}}\n\n"
    )
}

fn module(index: usize, types: usize) -> String {
    let mut out = format!("//! Generated module {index:02}.\n\n");
    for t in 0..types {
        out.push_str(&format!(
            "pub struct Node{index:02}_{t} {{\n    text: String,\n}}\n\n\
             impl Node{index:02}_{t} {{\n    \
             pub fn new(text: String) -> Self {{\n        Self {{ text }}\n    }}\n\n    \
             pub fn text(&self) -> &str {{\n        &self.text\n    }}\n}}\n\n"
        ));
    }
    out.push_str("pub fn execute(text: &str) -> usize {\n    text.len()\n}\n");
    out
}

/// A second declaration of the same name in the same file, under a nested
/// module: the two are different definitions, so the file scope must list
/// them rather than pick one.
const TWINS: &str = "\
pub struct Twin {
    text: String,
}

impl Twin {
    pub fn new(text: String) -> Self {
        Self { text }
    }
}

pub mod twins {
    pub struct Twin {
        pub text: String,
    }

    impl Twin {
        pub fn new(text: String) -> Self {
            Self { text }
        }
    }
}
";

fn scratch_root() -> PathBuf {
    let root = std::env::temp_dir().join(format!("structural-file-scope-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"file-scope\"\nversion = \"0.0.0\"\nedition = \"2021\"\npublish = false\n\n[workspace]\n",
    )
    .unwrap();
    let mut models = String::from("//! Generated model declarations.\n\n");
    for index in 0..MODELS {
        models.push_str(&model(index));
    }
    std::fs::write(root.join("src/models.rs"), models).unwrap();
    std::fs::write(root.join("src/twins.rs"), TWINS).unwrap();
    let mut lib = String::from("//! Generated crate root.\n\n");
    lib.push_str("pub mod models;\npub mod twins;\n");
    for index in 0..MODULES {
        lib.push_str(&format!("pub mod mod{index:02};\n"));
        std::fs::write(
            root.join(format!("src/mod{index:02}.rs")),
            module(index, MODULE_TYPES),
        )
        .unwrap();
    }
    lib.push_str(
        "\npub fn build() -> usize {\n    \
         let node = mod03::Node03_5::new(String::from(\"x\"));\n    \
         node.text().len()\n}\n",
    );
    std::fs::write(root.join("src/lib.rs"), lib).unwrap();
    // The Python fixture tree too, so a file scope goes through pyright.
    crate::fixtures_in(&root, &["python"]);
    std::fs::canonicalize(root).unwrap()
}

fn show(
    state: &structural::session::Session,
    symbol: Option<&str>,
    path: Option<&str>,
) -> anyhow::Result<String> {
    structural::tools::Show {
        symbol: symbol.map(str::to_string),
        path: path.map(str::to_string),
        anchor: None,
        focus: None,
        view: None,
    }
    .run(state)
}

fn uses(
    state: &structural::session::Session,
    symbol: Option<&str>,
    path: Option<&str>,
) -> anyhow::Result<String> {
    structural::tools::Uses {
        symbol: symbol.map(str::to_string),
        path: path.map(str::to_string),
        anchor: None,
        focus: None,
    }
    .run(state)
}

#[test]
#[ignore = "spawns rust-analyzer and pyright; run explicitly with -- --ignored"]
fn a_file_path_nominates_from_the_files_own_outline() {
    let root = scratch_root();
    let state = structural::session::Session::new(root.clone());
    let ok = |result: anyhow::Result<String>| result.unwrap_or_else(|error| panic!("{error:#}"));

    // Qualified names late in a file with more same-named definitions than
    // the index pool holds: the file's own outline is what resolves them.
    for type_name in ["Model139", "Model135", "Model130"] {
        let output = ok(show(
            &state,
            Some(&format!("{type_name}::new")),
            Some("src/models.rs"),
        ));
        assert!(
            output.starts_with("fn new · src/models.rs:"),
            "{type_name}::new must resolve from the file that declares it: {output}"
        );
        assert!(
            output.contains("pub fn new(text: String) -> Self {"),
            "{output}"
        );
    }

    // The bare name in that file: every definition the file declares is a
    // candidate, so the ambiguity listing is the file's, not a sample of it.
    let bare = ok(show(&state, Some("new"), Some("src/models.rs")));
    assert!(
        bare.starts_with(&format!("ambiguous: {MODELS} definitions of 'new'\n")),
        "every `new` in the file is a candidate: {bare}"
    );
    assert!(
        !bare.contains("note:"),
        "nothing was excluded from a file scope: {bare}"
    );

    // A qualifier the file does not name is listed from the file's own
    // candidates, with the container each one carries.
    let wrong = ok(show(&state, Some("Missing::new"), Some("src/models.rs")));
    assert!(
        wrong
            .starts_with("no 'new' is in a container ending in `Missing`; the 'new' candidates:\n"),
        "{wrong}"
    );
    assert!(wrong.contains("fn new · src/models.rs:"), "{wrong}");
    assert!(
        wrong.contains(" · in Model0\n"),
        "the listing carries each candidate's container: {wrong}"
    );
    assert!(
        wrong.contains(&format!("… {} more\n", MODELS - 20)),
        "the listing is the file's whole outline, not a 20-line sample: {wrong}"
    );

    // Two declarations of one name in a file are two definitions: listed,
    // not picked, whether the query names the type or only the method.
    for symbol in ["new", "Twin::new"] {
        let twins = ok(show(&state, Some(symbol), Some("src/twins.rs")));
        assert!(
            twins.starts_with(&format!("ambiguous: 2 definitions of '{symbol}'\n")),
            "both declarations of {symbol} in the file are listed: {twins}"
        );
    }

    // References resolve through the same file scope.
    let callers = ok(uses(&state, Some("Node03_5::new"), Some("src/mod03.rs")));
    assert!(
        callers.starts_with("fn new · src/mod03.rs:") && callers.contains("1 reference"),
        "{callers}"
    );
    assert!(callers.contains("src/lib.rs"), "{callers}");

    // On-disk edits are seen: the file is synced before the outline is read.
    let file = root.join("src/twins.rs");
    let mut text = std::fs::read_to_string(&file).unwrap();
    text.push_str(
        "\npub struct Fresh {\n    text: String,\n}\n\nimpl Fresh {\n    \
         pub fn new(text: String) -> Self {\n        Self { text }\n    }\n}\n",
    );
    std::fs::write(&file, text).unwrap();
    let added = ok(show(&state, Some("Fresh::new"), Some("src/twins.rs")));
    assert!(
        added.starts_with("fn new · src/twins.rs:"),
        "a type added on disk resolves: {added}"
    );
    let renamed = std::fs::read_to_string(&file)
        .unwrap()
        .replace("Fresh", "Fresher");
    std::fs::write(&file, renamed).unwrap();
    let gone = ok(show(&state, Some("Fresh::new"), Some("src/twins.rs")));
    assert!(
        gone.starts_with("no 'new' is in a container ending in `Fresh`"),
        "a renamed type must not resolve from a stale index: {gone}"
    );

    // pyright's outline nests the same way, so a file scope works there too.
    let python = ok(show(
        &state,
        Some("Config.doubled"),
        Some("python/helper.py"),
    ));
    assert!(
        python.starts_with("def doubled · python/helper.py:15\n"),
        "{python}"
    );
    assert!(python.contains("return self.limit * 2"), "{python}");

    drop(state);
    let _ = std::fs::remove_dir_all(&root);
}
