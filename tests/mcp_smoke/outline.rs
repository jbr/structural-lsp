//! `view = "outline"` past one page, over a directory, and without a
//! function's locals, through rust-analyzer and pyright (docs/tools.md).
//! Gated.

use std::path::PathBuf;

/// The Rust and Python fixtures, plus a Rust module of 60 structs with an
/// impl each — 300 outline symbols, past one 200-line page.
fn scratch_root() -> PathBuf {
    let root = std::env::temp_dir().join(format!("structural-outline-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    crate::fixtures_in(&root, &["rust", "python"]);
    let mut big = String::new();
    for i in 0..60 {
        big.push_str(&format!(
            "pub struct S{i} {{ pub a: u8, pub b: u8 }}\nimpl S{i} {{ pub fn f(&self) {{}} }}\n"
        ));
    }
    std::fs::write(root.join("rust/src/big.rs"), big).unwrap();
    std::fs::canonicalize(root).unwrap()
}

fn outline(
    state: &structural::session::Session,
    symbol: Option<&str>,
    path: &str,
    page: Option<usize>,
) -> anyhow::Result<String> {
    structural::tools::Definition {
        symbol: symbol.map(str::to_string),
        path: Some(path.to_string()),
        anchor: None,
        focus: None,
        view: Some("outline".to_string()),
        page,
        workspace: None,
        language: None,
    }
    .run(state)
}

#[test]
#[ignore = "spawns rust-analyzer and pyright; run explicitly with -- --ignored"]
fn outlines_page_survey_a_directory_and_leave_out_locals() {
    let root = scratch_root();
    let state = structural::session::Session::new(root.clone());

    // Page 1 of 300 symbols: 200 lines, and the argument that continues.
    let first = outline(&state, None, "rust/src/big.rs", None).unwrap();
    assert!(
        first.starts_with("rust/src/big.rs · 300 symbols\nstruct S0 · 1\n  field a · 1 · u8\n"),
        "{first}"
    );
    assert_eq!(first.lines().count(), 1 + 200 + 1, "{first}");
    assert!(
        first.ends_with("note: 100 more symbol(s); `page: 2` continues\n"),
        "{first}"
    );
    // Page 2 starts at symbol 201 — a field of `S40` — so the struct it
    // belongs to comes first as context; it is the last page.
    let second = outline(&state, None, "rust/src/big.rs", Some(2)).unwrap();
    assert!(
        second.starts_with("rust/src/big.rs · 300 symbols · page 2 of 2\nstruct S40 · 81\n"),
        "{second}"
    );
    assert!(second.ends_with("  fn f · 120 · fn(&self)\n"), "{second}");
    let past = outline(&state, None, "rust/src/big.rs", Some(3)).unwrap_err();
    assert_eq!(
        format!("{past:#}"),
        "`page: 3` is past the end: this outline has 2 page(s)"
    );

    // pyright lists `make`'s locals as its children; a file outline leaves
    // them out and says how many, and `make`'s own outline lists them.
    let module = outline(&state, None, "python/main.py", None).unwrap();
    assert!(module.starts_with("python/main.py · "), "{module}");
    assert!(module.contains(" locals not shown\n"), "{module}");
    assert!(module.contains("def make · 7\n"), "{module}");
    assert!(!module.contains("variable"), "{module}");
    let make = outline(&state, Some("make"), "python/main.py", None).unwrap();
    assert!(make.starts_with("def make · python/main.py:7 · "), "{make}");
    assert!(make.contains("\n  variable config · 9\n"), "{make}");

    // A directory: each served file, then its top-level items only.
    let directory = outline(&state, None, "python", None).unwrap();
    assert!(
        directory.starts_with("python · 2 files\npython/helper.py\n"),
        "{directory}"
    );
    assert!(directory.contains("\n  class Config · 8\n"), "{directory}");
    assert!(directory.contains("\npython/main.py\n  "), "{directory}");
    assert!(!directory.contains("__init__"), "{directory}");

    // The Rust crate: `Cargo.toml` is no server's, and says so once.
    let crate_dir = outline(&state, None, "rust", None).unwrap();
    assert!(
        crate_dir.starts_with("rust · 2 files\nrust/src/big.rs\n  struct S0 · 1\n"),
        "{crate_dir}"
    );
    assert!(
        crate_dir.contains("\nrust/src/lib.rs\n  struct Config · 5\n"),
        "{crate_dir}"
    );
    assert!(
        crate_dir.contains("no server here serves are not listed: ")
            && crate_dir.contains("`toml` (1)"),
        "{crate_dir}"
    );
}
