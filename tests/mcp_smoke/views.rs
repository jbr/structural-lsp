//! Slice 4 item 2: `definition` views on the fixtures (read-only, so the
//! fixtures themselves), through rust-analyzer, pyright and gopls. Gated.

use std::path::PathBuf;

fn scratch_root() -> PathBuf {
    let root = std::env::temp_dir().join(format!("structural-views-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    crate::fixtures_in(&root, &["rust", "python"]);
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

    // Implementations of a trait: the impl, with its line.
    let impls = show(
        &state,
        "implementations",
        Some("Label"),
        Some("rust"),
        None,
        None,
    );
    assert!(
        impls.starts_with("trait Label · rust/src/lib.rs:17 · 1 implementation · 1 file\n"),
        "{impls}"
    );
    assert!(
        impls.contains("rust/src/lib.rs\n  34 · impl Label for Config · impl Label for Config {\n"),
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
