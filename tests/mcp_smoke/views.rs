//! Slice 4 item 2: `definition` views on the fixtures (read-only, so the
//! fixtures themselves), through rust-analyzer, pyright and gopls. Gated.

use std::path::PathBuf;

fn scratch_root() -> PathBuf {
    let root = std::env::temp_dir().join(format!("structural-views-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    crate::fixtures_in(&root, &["rust", "python"]);
    // A test-only implementation of `Label`, after every line the cases
    // below cite, so `implementations` has a test to set apart.
    let lib = root.join("rust/src/lib.rs");
    let mut text = std::fs::read_to_string(&lib).unwrap();
    text.push_str(
        "\n#[cfg(test)]\nmod test {\n    struct Table;\n\n    \
         impl super::Label for Table {\n        fn label(&self) -> String {\n            \
         String::new()\n        }\n    }\n}\n",
    );
    std::fs::write(&lib, text).unwrap();
    std::fs::canonicalize(root).unwrap()
}

fn show(
    state: &structural::session::Session,
    view: &str,
    symbol: Option<&str>,
    path: Option<&str>,
    anchor: Option<&str>,
    focus: Option<&str>,
) -> String {
    structural::tools::Definition {
        symbol: symbol.map(str::to_string),
        path: path.map(str::to_string),
        anchor: anchor.map(str::to_string),
        focus: focus.map(str::to_string),
        view: Some(view.to_string()),
        page: None,
        workspace: None,
        language: None,
    }
    .run(state)
    .unwrap_or_else(|error| panic!("show view={view} {symbol:?} {path:?} {anchor:?}: {error:#}"))
}

#[test]
#[ignore = "spawns rust-analyzer and pyright; run explicitly with -- --ignored"]
fn views_answer_from_the_outline_hover_and_call_hierarchy() {
    let root = scratch_root();
    let state = structural::session::Session::new(root.clone());

    // A file outline: every item in order, methods under their impls.
    let outline = show(&state, "outline", None, Some("rust/src/lib.rs"), None, None);
    assert!(outline.starts_with("rust/src/lib.rs · "), "{outline}");
    let lines: Vec<&str> = outline.lines().collect();
    let index = |needle: &str| {
        lines
            .iter()
            .position(|line| line.starts_with(needle))
            .unwrap_or_else(|| panic!("{needle} in {outline}"))
    };
    assert!(
        index("struct Config · 5") < index("enum Mode · 11"),
        "{outline}"
    );
    assert!(
        index("impl Config · 21") < index("  fn new · 23"),
        "{outline}"
    );
    assert!(
        index("  fn new · 23") < index("  fn doubled · 28"),
        "{outline}"
    );
    assert!(
        index("fn choose · 43") < index("mod tests · 79"),
        "{outline}"
    );
    assert!(
        outline.contains("fn choose · 43 · fn(mode: Mode) -> usize\n"),
        "{outline}"
    );

    // A target's outline: its children.
    let children = show(&state, "outline", Some("Config"), Some("rust"), None, None);
    assert!(
        children.starts_with("struct Config · rust/src/lib.rs:5 · 1 children\n"),
        "{children}"
    );
    assert!(children.contains("  field limit · 7"), "{children}");
    let leaf = show(&state, "outline", Some("choose"), Some("rust"), None, None);
    assert!(leaf.ends_with("no children in the outline\n"), "{leaf}");

    // Implementations of a trait: the impl, with its line, and the
    // test-only impl set apart as `references` sets tests apart.
    let impls = show(
        &state,
        "implementations",
        Some("Label"),
        Some("rust"),
        None,
        None,
    );
    assert!(
        impls.starts_with(
            "trait Label · rust/src/lib.rs:17 · 2 implementations · 1 file\n\
             rust/src/lib.rs\n  34 · impl Label for Config · impl Label for Config {\n\
             tests (heuristic) · 1 implementation\n\
             rust/src/lib.rs\n  94 · test::impl super::Label for Table · impl super::Label for Table {\n"
        ),
        "{impls}"
    );

    // A derive is one implementation per derived trait, every one reported
    // at the `#[derive(…)]` line with nothing to tell them apart. The
    // header keeps the server's count and the rows are folded into the one
    // that names the traits, with a note for what was folded
    // (docs/tools.md).
    let derived = show(
        &state,
        "implementations",
        Some("Config"),
        Some("rust"),
        None,
        None,
    );
    let derive_rows = derived
        .lines()
        .filter(|line| line.contains("#[derive("))
        .count();
    assert_eq!(derive_rows, 1, "the derive is one row\n{derived}");
    assert!(
        derived.contains("#[derive(Debug, Clone)]"),
        "the row that survives names the traits\n{derived}"
    );
    assert!(
        derived.contains("note: 1 more reported at a line already shown"),
        "what was folded is stated\n{derived}"
    );

    // Calls both ways.
    let incoming = show(
        &state,
        "incoming-calls",
        Some("choose"),
        Some("rust"),
        None,
        None,
    );
    assert!(
        incoming.starts_with("fn choose · rust/src/lib.rs:43 · 1 caller\n"),
        "{incoming}"
    );
    assert!(
        incoming.contains("fn run · rust/src/lib.rs:54\n  62 · let offset = choose(Mode::Fast);\n"),
        "{incoming}"
    );
    // A caller in a test module is set apart under the heading
    // `references` uses.
    let test_callers = show(
        &state,
        "incoming-calls",
        Some("run"),
        Some("rust"),
        None,
        None,
    );
    assert!(
        test_callers.ends_with(
            "tests (heuristic) · 1 caller\nfn smoke · rust/src/lib.rs:84\n  86 · assert!(!run().is_empty());\n"
        ),
        "{test_callers}"
    );
    let outgoing = show(
        &state,
        "outgoing-calls",
        Some("run"),
        Some("rust"),
        None,
        None,
    );
    assert!(
        outgoing.starts_with("fn run · rust/src/lib.rs:54 · "),
        "{outgoing}"
    );
    assert!(
        outgoing.contains("fn new · rust/src/lib.rs:23\n  56 · let config = Config::new(3);\n"),
        "{outgoing}"
    );
    assert!(
        outgoing
            .contains("fn choose · rust/src/lib.rs:43\n  62 · let offset = choose(Mode::Fast);\n"),
        "{outgoing}"
    );
    // The standard library's callees follow the project's, one line each,
    // with no toolchain path.
    let (project, outside) = outgoing
        .split_once("outside the workspace · ")
        .unwrap_or_else(|| panic!("an outside section\n{outgoing}"));
    assert!(project.contains("fn choose · "), "{outgoing}");
    assert!(
        outside.contains("\nfn collect · called at 66\n"),
        "{outgoing}"
    );
    assert!(!outgoing.contains("rustlib"), "{outgoing}");
    let not_callable = show(
        &state,
        "incoming-calls",
        Some("Config"),
        Some("rust"),
        None,
        None,
    );
    assert!(not_callable.contains("not applicable"), "{not_callable}");

    // Types: a symbol's signature, and an expression's type over a focus.
    let signature = show(&state, "type", Some("choose"), Some("rust"), None, None);
    assert!(
        signature.starts_with("fn choose · rust/src/lib.rs:43\n"),
        "{signature}"
    );
    assert!(
        signature.contains("pub fn choose(mode: Mode) -> usize\n"),
        "{signature}"
    );
    let expression = show(
        &state,
        "type",
        None,
        Some("rust/src/lib.rs"),
        Some("let count = config.doubled();"),
        Some("config.doubled()"),
    );
    assert_eq!(
        expression, "type of `config.doubled()` · rust/src/lib.rs:58\nusize\n",
        "{expression}"
    );

    // rust-analyzer's own views.
    let expanded = show(
        &state,
        "expanded",
        None,
        Some("rust/src/lib.rs"),
        Some("        format!(\"limit={}\", self.limit)\n"),
        Some("format"),
    );
    assert!(
        expanded.starts_with("`format` · rust/src/lib.rs:38\nexpanded format\n"),
        "{expanded}"
    );
    assert!(expanded.contains("limit="), "{expanded}");
    let tests = show(
        &state,
        "related-tests",
        Some("run"),
        Some("rust"),
        None,
        None,
    );
    assert!(
        tests.starts_with("fn run · rust/src/lib.rs:54\n1 related test\n"),
        "{tests}"
    );
    assert!(
        tests.contains("test tests::smoke · rust/src/lib.rs:"),
        "{tests}"
    );
    assert!(tests.contains("\n  cargo test "), "{tests}");
    assert!(tests.contains("tests::smoke --exact"), "{tests}");

    // pyright: a signature, and the views it does not offer.
    let py_type = show(&state, "type", Some("choose"), Some("python"), None, None);
    assert!(
        py_type.starts_with("def choose · python/helper.py:28\n"),
        "{py_type}"
    );
    assert!(
        py_type.contains("def choose(flag: bool) -> int"),
        "{py_type}"
    );
    let py_calls = show(
        &state,
        "incoming-calls",
        Some("choose"),
        Some("python"),
        None,
        None,
    );
    assert!(
        py_calls.contains("def make · python/main.py:7\n  15 · offset = choose(True)\n"),
        "{py_calls}"
    );
    let py_expanded = show(
        &state,
        "expanded",
        Some("choose"),
        Some("python"),
        None,
        None,
    );
    assert_eq!(py_expanded, "view `expanded` is not supported by pyright\n");

    // An unknown view lists what exists.
    let unknown = show(&state, "nonsense", Some("choose"), Some("rust"), None, None);
    assert_eq!(
        unknown,
        "unknown view `nonsense`; views: definition, outline, implementations, incoming-calls, outgoing-calls, type, expanded, related-tests\n"
    );

    drop(state);
    let _ = std::fs::remove_dir_all(&root);
}

/// A workspace of a proc-macro crate (one derive) and a crate using it
/// beside two builtin derives.
fn derive_tree() -> PathBuf {
    let root = std::env::temp_dir().join(format!("structural-derive-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let aged = std::time::SystemTime::now() - std::time::Duration::from_secs(60);
    for (path, text) in [
        (
            "Cargo.toml",
            "[workspace]\nmembers = [\"pm\", \"app\"]\nresolver = \"2\"\n",
        ),
        (
            "pm/Cargo.toml",
            "[package]\nname = \"pm\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[lib]\nproc-macro = true\n",
        ),
        (
            "pm/src/lib.rs",
            "use proc_macro::TokenStream;\n\n#[proc_macro_derive(Hello)]\npub fn hello(_item: TokenStream) -> TokenStream {\n    \"pub fn hello_generated() -> u32 { 7 }\".parse().unwrap()\n}\n",
        ),
        (
            "app/Cargo.toml",
            "[package]\nname = \"app\"\nversion = \"0.0.0\"\nedition = \"2021\"\n\n[dependencies]\npm = { path = \"../pm\" }\n",
        ),
        (
            "app/src/lib.rs",
            "#[derive(Debug, Clone, pm::Hello)]\npub struct ToolCall {\n    pub name: String,\n}\n",
        ),
    ] {
        let file = root.join(path);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, text).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&file)
            .unwrap()
            .set_modified(aged)
            .unwrap();
    }
    std::fs::canonicalize(root).unwrap()
}

#[test]
#[ignore = "spawns rust-analyzer; run explicitly with -- --ignored"]
fn expanded_reaches_a_proc_macro_derive_and_explains_the_rest() {
    let root = derive_tree();
    let state = structural::session::Session::new(root.clone());
    let anchor = Some("#[derive(Debug, Clone, pm::Hello)]\npub struct ToolCall {");
    let at = |focus: &str| {
        show(
            &state,
            "expanded",
            None,
            Some("app/src/lib.rs"),
            anchor,
            Some(focus),
        )
    };

    // rust-analyzer expands a proc-macro derive at a token of its path.
    let hello = at("Hello");
    assert!(
        hello.starts_with("`Hello` · app/src/lib.rs:1\nexpanded pm::Hello\n"),
        "{hello}"
    );
    assert!(hello.contains("hello_generated"), "{hello}");

    // It returns nothing for a builtin derive, nor at `derive` itself, and
    // the answer says so rather than "not a macro call".
    let builtin = at("Debug");
    assert!(builtin.contains("builtin derive `Debug`"), "{builtin}");
    let keyword = at("derive");
    assert!(
        keyword.contains("expands a derive at the derived trait's name"),
        "{keyword}"
    );
    assert!(!keyword.contains("not a macro call"), "{keyword}");

    drop(state);
    let _ = std::fs::remove_dir_all(&root);
}
