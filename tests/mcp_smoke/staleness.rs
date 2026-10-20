//! Bare-name freshness. A bare-name query owns no file, so it is current
//! only because the query boundary brings each server up to the tree before
//! any index is read: every served file written or created since the
//! server's last census is pushed as content
//! ([`docs/correctness.md`](../docs/correctness.md)). Without that the
//! failure is silent — the honest-looking answer is "no such symbol".
//!
//! Each case is an edit followed immediately by the acceptance query: no
//! preliminary outline, nothing else reading the target file through
//! structural, no sleep. The only waits anywhere are the pipeline's own
//! (`sync_owned` + `wait_ready`). Before the census, both edit cases were
//! observed failing on rust-analyzer and macOS — FSEvents delivered the
//! edit's own event after the query was admitted — so these are the cases a
//! watcher alone cannot pass, run against each server family the census is
//! validated on (`process/testing.md`).
//!
//! Every case ends with a control: the same symbol through its file scope,
//! so a fixture that cannot resolve at all fails as itself rather than as a
//! freshness miss.

use std::path::PathBuf;

/// The two trees: a crate whose `lib.rs` declares `mod util;` and one
/// function, and a Python directory whose `main.py` holds one function and
/// `util.py` another. In both, the edits land in `util`, which the warmup
/// leaves unopened — the launcher's index warm opens the first served file
/// in sorted order, `lib.rs` or `main.py`, and the warmup query navigates
/// there and nowhere else.
#[derive(Clone, Copy)]
enum Language {
    Rust,
    Python,
}

impl Language {
    fn edited(self) -> &'static str {
        match self {
            Self::Rust => "src/util.rs",
            Self::Python => "util.py",
        }
    }

    fn created(self) -> &'static str {
        match self {
            Self::Rust => "src/probe.rs",
            Self::Python => "probe.py",
        }
    }

    /// The header a resolved function's answer starts with, up to its path.
    fn header(self, name: &str, path: &str) -> String {
        match self {
            Self::Rust => format!("fn {name} · {path}:"),
            Self::Python => format!("def {name} · {path}:"),
        }
    }

    /// A function returning `value`, as a line of this language.
    fn function(self, name: &str, value: u32) -> String {
        match self {
            Self::Rust => format!("pub fn {name}() -> usize {{ {value} }}\n"),
            Self::Python => format!("def {name}():\n    return {value}\n"),
        }
    }
}

fn scratch_root(language: Language, tag: &str) -> PathBuf {
    let root =
        std::env::temp_dir().join(format!("structural-staleness-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src")).unwrap();
    let files: &[(&str, &str)] = match language {
        Language::Rust => &[
            (
                "Cargo.toml",
                "[package]\nname = \"staleness\"\nversion = \"0.0.0\"\nedition = \"2021\"\npublish = false\n\n[workspace]\n",
            ),
            (
                "src/lib.rs",
                "pub mod util;\n\npub fn anchored_base() -> usize { 1 }\n",
            ),
            (
                "src/util.rs",
                "pub fn util_helper(x: usize) -> usize {\n    x + 1\n}\n",
            ),
        ],
        Language::Python => &[
            ("main.py", "def anchored_base():\n    return 1\n"),
            ("util.py", "def util_helper(x):\n    return x + 1\n"),
        ],
    };
    for (path, text) in files {
        let path = root.join(path);
        std::fs::write(&path, text).unwrap();
        // Dated a minute back, as a tree is that was not written as the
        // session started: a file stamped within the census's racy window
        // is pushed at the first query whatever else happens, and that
        // would own the edited file before the edit — passing the case by
        // ownership rather than by the census seeing the write.
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(std::time::SystemTime::now() - std::time::Duration::from_secs(60))
            .unwrap();
    }
    std::fs::canonicalize(root).unwrap()
}

/// Removes the scratch root when the test ends, however it ends. Declared
/// before the session, so unwinding drops the session first — its `Drop`
/// tears the servers down through the host lifecycle — and the directory
/// goes after.
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn show(state: &structural::session::Session, symbol: &str, path: Option<&str>) -> String {
    structural::tools::Definition {
        symbol: Some(symbol.to_string()),
        path: path.map(str::to_string),
        anchor: None,
        focus: None,
        view: None,
        page: None,
        workspace: None,
        language: None,
    }
    .run(state)
    .unwrap_or_else(|error| panic!("{symbol} ({path:?}): {error:#}"))
}

/// The one query that runs before any edit: it starts the server and waits
/// out its initial indexing. Its success is asserted, so a failed warmup
/// cannot vacuously set a case up.
fn warmup(state: &structural::session::Session) {
    let out = show(state, "anchored_base", None);
    assert!(
        out.contains("anchored_base ·"),
        "the warmup query must resolve the declaration it names: {out}"
    );
}

/// The edited file, appended to, then the bare name it now declares.
fn bare_name_sees_an_unopened_files_edit(language: Language, tag: &str) {
    let root = scratch_root(language, tag);
    let _scratch = Scratch(root.clone());
    let state = structural::session::Session::new(root.clone());
    warmup(&state);

    let file = root.join(language.edited());
    let mut text = std::fs::read_to_string(&file).unwrap();
    text.push('\n');
    text.push_str(&language.function("probe_marker_xyz", 42));
    std::fs::write(&file, text).unwrap();

    let header = language.header("probe_marker_xyz", language.edited());
    let bare = show(&state, "probe_marker_xyz", None);
    assert!(
        bare.starts_with(&header) && bare.contains("42"),
        "a bare-name query must see an unopened file's edit: {bare}"
    );
    let scoped = show(&state, "probe_marker_xyz", Some(language.edited()));
    assert!(scoped.starts_with(&header), "the control: {scoped}");

    drop(state);
}

/// A new file, then the bare name it declares. A Rust module is wired into
/// the crate in the same edit — `lib.rs` is owned, so that half is re-pushed
/// either way — and the case observes the new file, not crate membership.
fn bare_name_sees_a_newly_created_file(language: Language, tag: &str) {
    let root = scratch_root(language, tag);
    let _scratch = Scratch(root.clone());
    let state = structural::session::Session::new(root.clone());
    warmup(&state);

    std::fs::write(
        root.join(language.created()),
        language.function("probe_new_marker_xyz", 7),
    )
    .unwrap();
    if let Language::Rust = language {
        let lib = root.join("src/lib.rs");
        let text = std::fs::read_to_string(&lib).unwrap();
        std::fs::write(&lib, format!("pub mod probe;\n\n{text}")).unwrap();
    }

    let header = language.header("probe_new_marker_xyz", language.created());
    let bare = show(&state, "probe_new_marker_xyz", None);
    assert!(
        bare.starts_with(&header) && bare.contains('7'),
        "a bare-name query must see a newly created file: {bare}"
    );
    let scoped = show(&state, "probe_new_marker_xyz", Some(language.created()));
    assert!(scoped.starts_with(&header), "the control: {scoped}");

    drop(state);
}

#[test]
#[ignore = "spawns rust-analyzer; run explicitly with -- --ignored"]
fn rust_bare_name_sees_an_unopened_files_edit() {
    bare_name_sees_an_unopened_files_edit(Language::Rust, "rust-edit");
}

#[test]
#[ignore = "spawns rust-analyzer; run explicitly with -- --ignored"]
fn rust_bare_name_sees_a_newly_created_file() {
    bare_name_sees_a_newly_created_file(Language::Rust, "rust-create");
}

#[test]
#[ignore = "spawns pyright; run explicitly with -- --ignored"]
fn python_bare_name_sees_an_unopened_files_edit() {
    bare_name_sees_an_unopened_files_edit(Language::Python, "python-edit");
}

#[test]
#[ignore = "spawns pyright; run explicitly with -- --ignored"]
fn python_bare_name_sees_a_newly_created_file() {
    bare_name_sees_a_newly_created_file(Language::Python, "python-create");
}

/// A file deleted under the server is gone from its answers: the census
/// sees it missing, the owned copy is closed rather than failing the next
/// sync, and the server is told it was deleted, so the index cannot keep
/// answering from a file that is not there.
#[test]
#[ignore = "spawns rust-analyzer; run explicitly with -- --ignored"]
fn rust_bare_name_forgets_a_deleted_file() {
    let root = scratch_root(Language::Rust, "rust-delete");
    let _scratch = Scratch(root.clone());
    let state = structural::session::Session::new(root.clone());
    warmup(&state);
    let found = show(&state, "util_helper", None);
    assert!(
        found.starts_with("fn util_helper · src/util.rs:"),
        "{found}"
    );

    // `lib.rs` still declares `mod util;`: only the deletion can take
    // the symbol away, and the lookup above owned the file it was in.
    std::fs::remove_file(root.join("src/util.rs")).unwrap();

    let gone = show(&state, "util_helper", None);
    assert!(
        gone.starts_with("no symbol named 'util_helper'"),
        "a deleted file's symbol must not resolve: {gone}"
    );

    drop(state);
}
