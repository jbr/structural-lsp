//! What a workspace lays out, and what it says about what it did not.
//!
//! These build a `Workspace` directly and start nothing: laying out slots
//! is a decision about a tree, and it is testable without a process. The
//! launch-log half — that an unplanned server has no pid — is in
//! `host::tests`.

use super::*;
use std::sync::atomic::AtomicBool;

/// A scratch tree, and a `structural.toml` written verbatim.
fn workspace(name: &str, files: &[(&str, &str)], config: &str) -> Workspace {
    let root =
        std::env::temp_dir().join(format!("structural-workspace-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    for (path, contents) in files {
        let file = root.join(path);
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(file, contents).unwrap();
    }
    if !config.is_empty() {
        std::fs::write(root.join(crate::config::FILE_NAME), config).unwrap();
    }
    Workspace::new(root, Arc::new(AtomicBool::new(false)))
}

fn slot_names(workspace: &Workspace) -> Vec<&str> {
    workspace.slots().iter().map(|slot| slot.name()).collect()
}

#[test]
fn a_tree_lays_out_only_the_slots_its_languages_call_for() {
    let rust = workspace("rust-only", &[("Cargo.toml", ""), ("src/lib.rs", "")], "");
    assert_eq!(slot_names(&rust), ["rust-analyzer"]);

    let python = workspace("python-only", &[("pyproject.toml", ""), ("app.py", "")], "");
    assert_eq!(slot_names(&python), ["pyright"]);

    let both = workspace(
        "both",
        &[("Cargo.toml", ""), ("src/lib.rs", ""), ("app.py", "")],
        "",
    );
    // Plan order — by language, then by server — not configuration order:
    // slots come from plans, and a plan's order is the stated one
    // (`docs/discovery.md`).
    assert_eq!(slot_names(&both), ["pyright", "rust-analyzer"]);
}

#[test]
fn an_empty_tree_lays_out_nothing() {
    // Not an error and not every server: a directory that says nothing
    // about itself gets no slots, and the message a query receives is the
    // one below.
    let empty = workspace("empty", &[("README.md", "")], "");
    assert!(slot_names(&empty).is_empty());
}

#[test]
fn a_disabled_builtin_lays_out_no_slot() {
    // The capability `structural.toml` did not have: `adapters` always
    // emitted both built-ins, so a project could override a server but
    // never decline one.
    let disabled = workspace(
        "disabled",
        &[("Cargo.toml", ""), ("src/lib.rs", "")],
        "[servers.rust-analyzer]\ndisabled = true\n",
    );
    assert!(slot_names(&disabled).is_empty());
}

#[test]
fn a_disabled_entry_is_never_validated() {
    // `disabled = true` on a generic server with no `command` is a
    // statement about this project, not an incomplete definition. Before,
    // the same entry would have failed the whole config and left the
    // workspace with no servers at all and a parse error on every call.
    let disabled = workspace(
        "disabled-generic",
        &[("Cargo.toml", ""), ("src/lib.rs", "")],
        "[servers.someserver]\ndisabled = true\n",
    );
    assert_eq!(disabled.config_error(), None);
    assert_eq!(slot_names(&disabled), ["rust-analyzer"]);
}

#[test]
fn a_configured_server_is_planned_from_the_languages_it_declares() {
    let go = workspace(
        "go",
        &[("go.mod", ""), ("main.go", "")],
        "[servers.gopls]\ncommand = [\"gopls\"]\nlanguages = [\"go\"]\n",
    );
    assert_eq!(
        slot_names(&go),
        ["gopls"],
        "the catalogue knows neither Go nor gopls; the override supplies both"
    );
}

#[test]
fn a_configured_server_whose_language_is_absent_lays_out_no_slot() {
    // Naming a server in configuration is not the same as asking for it
    // here. This is the case that used to start an unrelated server in
    // every project that had ever configured one.
    let go = workspace(
        "go-absent",
        &[("Cargo.toml", ""), ("src/lib.rs", "")],
        "[servers.gopls]\ncommand = [\"gopls\"]\nlanguages = [\"go\"]\n",
    );
    assert_eq!(slot_names(&go), ["rust-analyzer"]);
}

#[test]
fn a_file_no_slot_serves_is_told_what_is_not_here_and_why() {
    // The gap laying out slots by plan opens: pyright *is* configured,
    // and a tree with no Python has no slot for it, so a `.py` file that
    // appears later would otherwise get "no language server serves `py`
    // files" — true, and useless, because it does not say that a server
    // for it exists and was left out.
    let rust = workspace(
        "late-python",
        &[("Cargo.toml", ""), ("src/lib.rs", "")],
        "",
    );
    let late = rust.root().join("late.py");
    std::fs::write(&late, "def a():\n    pass\n").unwrap();

    let error = rust
        .select(Some(&late))
        .expect_err("no slot serves python here");
    let text = format!("{error:#}");
    assert!(text.contains("serves `py` files"), "{text}");
    assert!(
        text.contains("not here: pyright"),
        "the server that would have served it is named: {text}"
    );
    assert!(
        text.contains("nothing in the tree calls for python"),
        "and why it was left out: {text}"
    );
}

#[test]
fn two_servers_for_one_language_are_one_slot_with_a_ladder() {
    // A plan carries every candidate for its language, and the slot it
    // becomes holds all of them: the alternatives are rungs to walk when
    // the preferred one will not start, not servers to run alongside it.
    // Neither of these is measured and neither has consensus, so the rank
    // falls through to name ascending — which is the stated order the
    // ladder is asserted against (`docs/discovery.md`).
    let ladder = workspace(
        "ladder",
        &[("Cargo.toml", ""), ("src/lib.rs", "")],
        "[servers.rust-analyzer]\ndisabled = true\n\n\
         [servers.b-second]\ncommand = [\"b\"]\nlanguages = [\"rust\"]\n\n\
         [servers.a-first]\ncommand = [\"a\"]\nlanguages = [\"rust\"]\n",
    );
    assert_eq!(slot_names(&ladder), ["a-first"], "one slot, not two");
    assert_eq!(
        ladder.slots()[0].candidate_names(),
        ["a-first", "b-second"],
        "and it holds the whole ladder, in rank order"
    );
}

#[test]
fn one_server_planned_for_two_languages_is_one_slot() {
    // One server is one process however many languages it was planned
    // for, so two plans naming it become one slot. The ladder is then the
    // intersection of the two plans': falling back to a candidate serving
    // only one of the languages would quietly drop the other, and here
    // rust-analyzer and pyright each serve one, so neither survives.
    let poly = workspace(
        "polyglot-server",
        &[("Cargo.toml", ""), ("src/lib.rs", ""), ("app.py", "")],
        "[servers.poly]\ncommand = [\"poly\"]\nlanguages = [\"rust\", \"python\"]\n",
    );
    assert_eq!(slot_names(&poly), ["poly"]);
    assert_eq!(
        poly.slots()[0].candidate_names(),
        ["poly"],
        "a fallback that served only one of the two would answer for less \
         than the slot was laid out for"
    );
}
