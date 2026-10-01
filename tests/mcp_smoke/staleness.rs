//! Bare-name staleness. A bare-name query owns no file, so
//! `workspace/symbol` can answer without having seen a recent external edit
//! to a file structural has never opened, and the failure is silent — the
//! honest-looking answer is "no such symbol"
//! ([`docs/correctness.md`](../docs/correctness.md)).
//!
//! The tests are kept apart by what each may assert:
//!
//! * The active test asserts what is true today and stays true whenever the
//!   bare-name gap closes (`docs/correctness.md`): a file-scoped query is
//!   current for a file nobody has opened, and a bare-name query is current
//!   once a query has owned the file. It never asserts the miss.
//! * The pending tests assert the *desired* behavior — an immediate
//!   bare-name query after an unopened-file edit, and after a newly-created
//!   file. They are `#[ignore]`d and compiled only under
//!   `--features pending-acceptance`, so neither `cargo test` nor the
//!   standing `cargo test -- --ignored` runs them; the explicit run is
//!   `cargo test --features pending-acceptance -- --ignored -- pending_`
//!   (`process/testing.md`). Their ignore reasons stay pending until each
//!   case's outcome is observed: a failure keeps the case pending, and a
//!   pass is one observation — promotion to ordinary coverage happens only
//!   when the unit that makes the case hold has validated it per server,
//!   with wording that says what was observed, never from one run alone.
//!
//! No test sleeps, and the acceptance query is the *first* query after the
//! edit: nothing outlines the target file, nothing else reads it through
//! structural, and the only waits anywhere are the pipeline's own
//! (`sync_owned` + `wait_ready`).

use std::path::PathBuf;

/// A scratch crate whose root owns nothing yet: `lib.rs` declares `mod util;`
/// and one function; `util.rs` holds another and is the file the edits land
/// in, unopened, after the warmup has run.
fn scratch_root(tag: &str) -> PathBuf {
    let root =
        std::env::temp_dir().join(format!("structural-staleness-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"staleness\"\nversion = \"0.0.0\"\nedition = \"2021\"\npublish = false\n\n[workspace]\n",
    )
    .unwrap();
    std::fs::write(
        root.join("src/lib.rs"),
        "pub mod util;\n\npub fn anchored_base() -> usize { 1 }\n",
    )
    .unwrap();
    std::fs::write(
        root.join("src/util.rs"),
        "pub fn util_helper(x: usize) -> usize {\n    x + 1\n}\n",
    )
    .unwrap();
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

fn show(
    state: &structural::session::Session,
    symbol: Option<&str>,
    path: Option<&str>,
) -> anyhow::Result<String> {
    structural::tools::Definition {
        symbol: symbol.map(str::to_string),
        path: path.map(str::to_string),
        anchor: None,
        focus: None,
        view: None,
    }
    .run(state)
}

/// The one query that runs before any edit: it starts the server, waits out
/// its initial indexing, and owns `lib.rs` alone — `util.rs` is never
/// opened, because the candidate navigates in `lib.rs` and nothing else is
/// nominated. Its success is asserted, so a failed warmup cannot vacuously
/// set a case up.
fn warmup(state: &structural::session::Session) {
    let out = show(state, Some("anchored_base"), None)
        .unwrap_or_else(|error| panic!("the warmup query must resolve: {error:#}"));
    assert!(
        out.starts_with("fn anchored_base ·"),
        "the warmup query must resolve the declaration it names: {out}"
    );
}

/// Append a function the server's index has never heard of to the file the
/// warmup did not open.
fn append_probe(file: &std::path::Path, declaration: &str, body: &str) {
    let mut text = std::fs::read_to_string(file).unwrap();
    text.push_str(&format!("\n{declaration} {{ {body} }}\n"));
    std::fs::write(file, text).unwrap();
}

#[test]
#[ignore = "spawns rust-analyzer; run explicitly with -- --ignored"]
fn a_file_scope_query_is_current_for_an_unopened_file_and_the_bare_name_once_owned() {
    let root = scratch_root("file-scope-current");
    let _scratch = Scratch(root.clone());
    let state = structural::session::Session::new(root.clone());
    warmup(&state);

    let file = root.join("src/util.rs");
    append_probe(&file, "pub fn probe_marker_xyz() -> usize", "42");

    // First query after the edit, and the file was never opened: a file
    // scope syncs the file itself before it outlines, so the answer is the
    // server's view of the text just read.
    let scoped = show(&state, Some("probe_marker_xyz"), Some("src/util.rs"))
        .unwrap_or_else(|error| panic!("{error:#}"));
    assert!(
        scoped.starts_with("fn probe_marker_xyz · src/util.rs:"),
        "a file-scoped query must see an unopened file's edit: {scoped}"
    );
    assert!(scoped.contains("42"), "{scoped}");

    // That query owned the file, so the bare name — the one path that can
    // miss a recent edit — is current now, without any further query having
    // touched the file.
    let bare =
        show(&state, Some("probe_marker_xyz"), None).unwrap_or_else(|error| panic!("{error:#}"));
    assert!(
        bare.starts_with("fn probe_marker_xyz ·"),
        "once the file is owned, a bare-name query must be current: {bare}"
    );
    assert!(bare.contains("42"), "{bare}");

    drop(state);
}

#[test]
#[cfg(feature = "pending-acceptance")]
#[ignore = "PENDING freshness acceptance, observed failing (docs/correctness.md, process/testing.md): the miss is the honest-looking 'no such symbol'; run explicitly with cargo test --features pending-acceptance -- --ignored -- pending_"]
fn pending_bare_name_sees_an_unopened_files_edit() {
    let root = scratch_root("pending-edit");
    let _scratch = Scratch(root.clone());
    let state = structural::session::Session::new(root.clone());
    warmup(&state);

    append_probe(
        &root.join("src/util.rs"),
        "pub fn probe_marker_xyz() -> usize",
        "42",
    );

    // First query after the edit: the acceptance query itself, nothing
    // else.
    let bare =
        show(&state, Some("probe_marker_xyz"), None).unwrap_or_else(|error| panic!("{error:#}"));
    assert!(
        bare.starts_with("fn probe_marker_xyz ·"),
        "a bare-name query must see an unopened file's edit: {bare}"
    );
    assert!(bare.contains("42"), "{bare}");

    // Runs only once the acceptance above holds: the same symbol through its
    // file scope, so a promoted case also proves the fixture resolves once
    // the file is synced, and the pending result was about freshness, never
    // about a broken fixture.
    let scoped = show(&state, Some("probe_marker_xyz"), Some("src/util.rs"))
        .unwrap_or_else(|error| panic!("{error:#}"));
    assert!(
        scoped.starts_with("fn probe_marker_xyz · src/util.rs:"),
        "the control: the same symbol must resolve through its file scope: {scoped}"
    );
    assert!(scoped.contains("42"), "{scoped}");

    drop(state);
}

#[test]
#[cfg(feature = "pending-acceptance")]
#[ignore = "PENDING freshness acceptance, observed failing (docs/correctness.md, process/testing.md): the miss is the honest-looking 'no such symbol' even though the owned crate root was re-fed; run explicitly with cargo test --features pending-acceptance -- --ignored -- pending_"]
fn pending_bare_name_sees_a_newly_created_file() {
    let root = scratch_root("pending-create");
    let _scratch = Scratch(root.clone());
    let state = structural::session::Session::new(root.clone());
    warmup(&state);

    std::fs::write(
        root.join("src/probe.rs"),
        "pub fn probe_new_marker_xyz() -> usize { 7 }\n",
    )
    .unwrap();
    // The module wiring is part of the same creation edit, so the acceptance
    // tests observing the new file, not crate membership — and `lib.rs` is
    // owned, so its half of the edit is re-pushed while the new file's
    // contents have still never been seen.
    let lib = root.join("src/lib.rs");
    let text = std::fs::read_to_string(&lib).unwrap();
    std::fs::write(&lib, format!("pub mod probe;\n\n{text}")).unwrap();

    // First query after the edit: the acceptance query itself, nothing
    // else.
    let bare = show(&state, Some("probe_new_marker_xyz"), None)
        .unwrap_or_else(|error| panic!("{error:#}"));
    assert!(
        bare.starts_with("fn probe_new_marker_xyz ·"),
        "a bare-name query must see a newly created file: {bare}"
    );
    assert!(bare.contains("7"), "{bare}");

    // Runs only once the acceptance above holds: the same symbol through its
    // file scope, so a promoted case also proves the fixture and the module
    // wiring resolve once the file is synced, and the pending result was
    // about freshness, never about a broken fixture.
    let scoped = show(&state, Some("probe_new_marker_xyz"), Some("src/probe.rs"))
        .unwrap_or_else(|error| panic!("{error:#}"));
    assert!(
        scoped.starts_with("fn probe_new_marker_xyz · src/probe.rs:"),
        "the control: the same symbol must resolve through its file scope: {scoped}"
    );
    assert!(scoped.contains("7"), "{scoped}");

    drop(state);
}
