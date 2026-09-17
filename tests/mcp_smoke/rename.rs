//! Slice 4 item 3: `action` rename on scratch copies (mutation never
//! touches the fixtures). Gated: spawns rust-analyzer and pyright.

use mcplease::traits::Tool;
use std::path::PathBuf;

/// Both fixtures under one root, the Rust one with a second module so a
/// module rename has a file to move. One root per `tag`: these tests run in
/// parallel and each one deletes its root on entry and on exit, so a shared
/// root makes them delete each other's tree mid-run — which surfaces as a
/// stale-server error, not as the collision it is.
fn scratch_root(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("structural-rename-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    crate::fixtures_in(&root, &["rust", "python"]);
    let lib = root.join("rust/src/lib.rs");
    let text = std::fs::read_to_string(&lib).unwrap();
    std::fs::write(
        &lib,
        format!(
            "mod util;\n\n{text}\npub fn uses_helper(x: usize) -> usize {{ util::helper(x) }}\n"
        ),
    )
    .unwrap();
    std::fs::write(
        root.join("rust/src/util.rs"),
        "pub fn helper(x: usize) -> usize {\n    x + 1\n}\n",
    )
    .unwrap();
    std::fs::canonicalize(root).unwrap()
}

fn action(
    state: &mut structural::session::Session,
    symbol: &str,
    path: &str,
    new_name: &str,
    mode: Option<&str>,
    digest: Option<&str>,
) -> String {
    structural::tools::Action {
        action: "rename".to_string(),
        symbol: Some(symbol.to_string()),
        path: Some(path.to_string()),
        anchor: None,
        focus: None,
        new_name: Some(new_name.to_string()),
        mode: mode.map(str::to_string),
        digest: digest.map(str::to_string),
    }
    .execute(state, &mcplease::types::RequestContext::default())
    .unwrap_or_else(|error| panic!("rename {symbol} → {new_name} ({mode:?}): {error:#}"))
}

fn show(state: &mut structural::session::Session, symbol: &str, path: &str) -> String {
    structural::tools::Show {
        symbol: Some(symbol.to_string()),
        path: Some(path.to_string()),
        anchor: None,
        focus: None,
        view: None,
    }
    .execute(state, &mcplease::types::RequestContext::default())
    .unwrap_or_else(|error| panic!("show {symbol}: {error:#}"))
}

fn digest_of(preview: &str) -> String {
    preview
        .lines()
        .find_map(|line| line.strip_prefix("digest: "))
        .unwrap_or_else(|| panic!("no digest in {preview}"))
        .to_string()
}

/// The real pipeline — progress accounting plus the adapter's readiness —
/// against the real rust-analyzer: the rename succeeds on code, and the
/// adapter's caveat rides on the preview and the applied summary. Whether
/// this version's edit happens to rewrite the doc link is not asserted:
/// the caveat says "may" precisely so the test holds either way.
#[test]
#[ignore = "spawns rust-analyzer and pyright; run explicitly with -- --ignored"]
fn the_rename_answer_carries_the_server_caveat() {
    let root = scratch_root("caveat");
    // A doc-comment intra-doc link to the symbol being renamed, so the
    // caveat has something to point at.
    let lib = root.join("rust/src/lib.rs");
    let text = std::fs::read_to_string(&lib).unwrap();
    std::fs::write(
        &lib,
        format!(
            "{text}\n/// Links [`choose`] in the docs.\n\
             pub fn doc_link_site() {{ let _ = choose(Mode::Fast); }}\n"
        ),
    )
    .unwrap();
    let mut state = structural::session::Session::new(root.clone());

    let preview = action(&mut state, "choose", "rust", "pick", None, None);
    assert!(
        preview.contains(
            "note: rust-analyzer rename may leave intra-doc links unchanged; check doc comments for `choose`.\n"
        ),
        "{preview}"
    );
    assert!(preview.contains("digest: "), "{preview}");
    let digest = digest_of(&preview);

    let applied = action(
        &mut state,
        "choose",
        "rust",
        "pick",
        Some("apply"),
        Some(&digest),
    );
    assert!(
        applied.starts_with("applied: rename choose → pick"),
        "{applied}"
    );
    assert!(
        applied.ends_with(
            "note: rust-analyzer rename may leave intra-doc links unchanged; check doc comments for `choose`.\n"
        ),
        "{applied}"
    );
    let text = std::fs::read_to_string(&lib).unwrap();
    assert!(
        text.contains("pub fn pick(mode: Mode) -> usize {") && text.contains("= pick(Mode::Fast);"),
        "the code rename succeeded: {text}"
    );

    drop(state);
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
#[ignore = "spawns rust-analyzer and pyright; run explicitly with -- --ignored"]
fn rename_previews_then_applies_with_a_matching_digest() {
    let root = scratch_root("digest");
    let mut state = structural::session::Session::new(root.clone());

    // Preview: the definition and the call, both lines shown, nothing
    // written.
    let preview = action(&mut state, "choose", "rust", "pick", None, None);
    assert!(
        preview.starts_with(
            "rename choose → pick · fn choose · rust/src/lib.rs:45 · 2 edits · 1 file\n"
        ),
        "{preview}"
    );
    assert!(
        preview.contains(
            "  45: pub fn choose(mode: Mode) -> usize {\n      pub fn pick(mode: Mode) -> usize {\n"
        ),
        "{preview}"
    );
    assert!(
        preview.contains(
            "  64:     let offset = choose(Mode::Fast);\n          let offset = pick(Mode::Fast);\n"
        ),
        "{preview}"
    );
    let lib = root.join("rust/src/lib.rs");
    assert!(
        std::fs::read_to_string(&lib)
            .unwrap()
            .contains("pub fn choose"),
        "a preview writes nothing"
    );
    let digest = digest_of(&preview);

    // Apply without a digest, or with a wrong one: a fresh preview and no
    // write.
    let no_digest = action(&mut state, "choose", "rust", "pick", Some("apply"), None);
    assert!(
        no_digest.starts_with("nothing applied: no digest given; a fresh preview follows\n"),
        "{no_digest}"
    );
    let wrong = action(
        &mut state,
        "choose",
        "rust",
        "pick",
        Some("apply"),
        Some("0000000000000000"),
    );
    assert!(
        wrong.starts_with("nothing applied: the digest does not match"),
        "{wrong}"
    );
    assert!(
        std::fs::read_to_string(&lib)
            .unwrap()
            .contains("pub fn choose")
    );

    // Apply with the digest: written, and the servers see it.
    let applied = action(
        &mut state,
        "choose",
        "rust",
        "pick",
        Some("apply"),
        Some(&digest),
    );
    assert_eq!(
        applied,
        "applied: rename choose → pick · fn choose · rust/src/lib.rs:45 · 2 edits · 1 file\n\
         rust/src/lib.rs · 2 edits\n\
         note: rust-analyzer rename may leave intra-doc links unchanged; \
         check doc comments for `choose`.\n"
    );
    let text = std::fs::read_to_string(&lib).unwrap();
    assert!(
        text.contains("pub fn pick(mode: Mode) -> usize {"),
        "{text}"
    );
    assert!(text.contains("let offset = pick(Mode::Fast);"), "{text}");
    assert!(
        !text.contains("fn choose") && !text.contains("= choose("),
        "{text}"
    );
    let after = show(&mut state, "pick", "rust");
    assert!(
        after.starts_with("fn pick · rust/src/lib.rs:45\n"),
        "{after}"
    );
    let gone = show(&mut state, "choose", "rust");
    assert!(
        gone.starts_with("no symbol named 'choose'")
            || gone.starts_with("no exact match for 'choose'"),
        "{gone}"
    );

    // The old digest is stale now: applying it again changes nothing.
    let stale = action(
        &mut state,
        "pick",
        "rust",
        "choose",
        Some("apply"),
        Some(&digest),
    );
    assert!(
        stale.starts_with("nothing applied: the digest does not match"),
        "{stale}"
    );

    // A struct: the impl headers, the constructor call and the re-export
    // are rewritten; `Self` sites need nothing.
    let config = action(&mut state, "Config", "rust", "Settings", None, None);
    assert!(config.contains("impl Settings {\n"), "{config}");
    assert!(config.contains("impl Label for Settings {\n"), "{config}");
    assert!(
        config.contains("let config = Settings::new(3);\n"),
        "{config}"
    );
    assert!(config.contains("pub use super::Settings;\n"), "{config}");
    let config_applied = action(
        &mut state,
        "Config",
        "rust",
        "Settings",
        Some("apply"),
        Some(&digest_of(&config)),
    );
    assert!(
        config_applied.starts_with("applied: rename Config → Settings"),
        "{config_applied}"
    );
    assert!(
        std::fs::read_to_string(&lib)
            .unwrap()
            .contains("pub struct Settings {")
    );

    // A module: its file moves and the `mod` line and paths follow.
    let module = action(&mut state, "util", "rust", "helpers", None, None);
    assert!(
        module.contains("  1: mod util;\n     mod helpers;\n"),
        "{module}"
    );
    assert!(module.contains("util::helper(x)"), "{module}");
    assert!(
        module.contains(&format!(
            "move {} → {}\n",
            root.join("rust/src/util.rs").display(),
            root.join("rust/src/helpers.rs").display()
        )),
        "{module}"
    );
    let module_applied = action(
        &mut state,
        "util",
        "rust",
        "helpers",
        Some("apply"),
        Some(&digest_of(&module)),
    );
    assert!(module_applied.contains("moved "), "{module_applied}");
    assert!(root.join("rust/src/helpers.rs").is_file() && !root.join("rust/src/util.rs").exists());
    let helper = show(&mut state, "helper", "rust");
    assert!(
        helper.starts_with("fn helper · rust/src/helpers.rs:1\n"),
        "{helper}"
    );

    // pyright: across two files.
    let py = action(&mut state, "choose", "python", "select", None, None);
    assert!(
        py.starts_with(
            "rename choose → select · def choose · python/helper.py:28 · 3 edits · 2 files\n"
        ),
        "{py}"
    );
    assert!(py.contains("python/main.py\n"), "{py}");
    assert!(
        py.contains("from helper import DEFAULT_LIMIT, Config, Label, select, identity\n"),
        "{py}"
    );
    assert!(py.contains("offset = select(True)\n"), "{py}");
    let py_applied = action(
        &mut state,
        "choose",
        "python",
        "select",
        Some("apply"),
        Some(&digest_of(&py)),
    );
    assert!(
        py_applied.starts_with("applied: rename choose → select"),
        "{py_applied}"
    );
    assert!(
        std::fs::read_to_string(root.join("python/main.py"))
            .unwrap()
            .contains("offset = select(True)")
    );
    let py_after = show(&mut state, "select", "python");
    assert!(
        py_after.starts_with("def select · python/helper.py:28\n"),
        "{py_after}"
    );

    // Argument errors.
    let same = structural::tools::Action {
        action: "rename".to_string(),
        symbol: Some("pick".to_string()),
        path: Some("rust".to_string()),
        anchor: None,
        focus: None,
        new_name: Some("pick".to_string()),
        mode: None,
        digest: None,
    }
    .execute(&mut state, &mcplease::types::RequestContext::default());
    assert!(same.unwrap_err().to_string().contains("already the name"));
    let unknown = structural::tools::Action {
        action: "explode".to_string(),
        symbol: Some("pick".to_string()),
        path: None,
        anchor: None,
        focus: None,
        new_name: None,
        mode: None,
        digest: None,
    }
    .execute(&mut state, &mcplease::types::RequestContext::default());
    assert!(unknown.unwrap_err().to_string().contains("unknown action"));

    drop(state);
    let _ = std::fs::remove_dir_all(&root);
}
