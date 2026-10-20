//! Root choice on constructed trees. Every case here is one the rule was
//! chosen to answer a particular way, so a change in the rule fails a named
//! test rather than a count.
//!
//! The cargo-workspace case mirrors the fixture that
//! `reference/server-root-discovery.md` measured rust-analyzer against.

use super::*;

/// A scratch tree: directories to create, then files to write (empty).
fn tree(name: &str, directories: &[&str], files: &[&str]) -> PathBuf {
    let root = std::env::temp_dir().join(format!("structural-roots-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    for directory in directories {
        std::fs::create_dir_all(root.join(directory)).unwrap();
    }
    for file in files {
        let path = root.join(file);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, "").unwrap();
    }
    std::fs::canonicalize(root).unwrap()
}

#[test]
fn the_outermost_marker_wins_not_the_nearest() {
    // A cargo workspace: a manifest at the top and one per member. Started
    // inside a member, the key must be the workspace — otherwise alpha and
    // beta key apart and each gets its own server, which is the sharing bug
    // this rule exists to close.
    let root = tree(
        "workspace",
        &[".git", "crates/alpha/src", "crates/beta/src"],
        &[
            "Cargo.toml",
            "crates/alpha/Cargo.toml",
            "crates/beta/Cargo.toml",
        ],
    );

    for start in ["crates/alpha", "crates/alpha/src", "crates/beta"] {
        let ancestry = Ancestry::gather(&root.join(start));
        let chosen = ancestry.root();
        assert_eq!(
            chosen.path, root,
            "from {start}, the outermost Cargo.toml is the workspace's"
        );
        assert_eq!(
            chosen.reason,
            RootReason::Marker {
                file: "Cargo.toml",
                bounded_by: Some(root.clone()),
            }
        );
    }

    // Two members of one workspace therefore key the same, which is the
    // whole point: one server, two scopes.
    assert_eq!(
        Ancestry::gather(&root.join("crates/alpha")).root().path,
        Ancestry::gather(&root.join("crates/beta")).root().path
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_repository_holding_unrelated_projects_is_not_the_root() {
    // No manifest at the top: `backend` and `tools` are separate projects
    // and must key apart. Answering with the repository would fuse them
    // into one server told to index everything.
    let root = tree(
        "unrelated",
        &[".git", "backend/src", "tools/src"],
        &["backend/Cargo.toml", "tools/Cargo.toml"],
    );

    let backend = Ancestry::gather(&root.join("backend/src")).root();
    let tools = Ancestry::gather(&root.join("tools/src")).root();
    assert_eq!(backend.path, root.join("backend"));
    assert_eq!(tools.path, root.join("tools"));
    assert_ne!(
        backend.path, tools.path,
        "unrelated projects under one repository do not share a key"
    );

    // The repository bounded the search; it did not supply the answer.
    assert_eq!(
        backend.reason,
        RootReason::Marker {
            file: "Cargo.toml",
            bounded_by: Some(root.clone()),
        }
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_vcs_root_bounds_the_search() {
    // A manifest *above* the repository is out of the span: a checkout
    // inside someone else's project does not adopt that project's root.
    let root = tree(
        "bounded",
        &["outer/inner/.git", "outer/inner/src"],
        &["outer/Cargo.toml"],
    );

    let chosen = Ancestry::gather(&root.join("outer/inner/src")).root();
    assert_eq!(
        chosen.path,
        root.join("outer/inner/src"),
        "the Cargo.toml above the repository is out of the span, so the          working directory is the floor"
    );
    assert_eq!(
        chosen.reason,
        RootReason::WorkingDirectory {
            vcs_root: Some(root.join("outer/inner")),
        }
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn the_nearest_repository_wins_so_a_submodule_is_its_own_span() {
    let root = tree(
        "submodule",
        &[".git", "vendor/dep/.git", "vendor/dep/src"],
        &["Cargo.toml", "vendor/dep/Cargo.toml"],
    );

    let chosen = Ancestry::gather(&root.join("vendor/dep/src")).root();
    assert_eq!(chosen.path, root.join("vendor/dep"));
    assert_eq!(
        chosen.reason,
        RootReason::Marker {
            file: "Cargo.toml",
            bounded_by: Some(root.join("vendor/dep")),
        },
        "the nested checkout bounds its own span"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_worktree_nested_in_its_own_repository_is_its_own_span() {
    // The outer checkout's `.git` is a directory; the worktree's is a file
    // naming it, as `git worktree add` writes it.
    let root = tree(
        "worktree",
        &[".git", ".claude/worktrees/agent/src"],
        &["Cargo.toml", ".claude/worktrees/agent/Cargo.toml"],
    );
    std::fs::write(
        root.join(".claude/worktrees/agent/.git"),
        "gitdir: ../../../.git/worktrees/agent\n",
    )
    .unwrap();

    let worktree = root.join(".claude/worktrees/agent");
    let chosen = Ancestry::gather(&worktree.join("src")).root();
    assert_eq!(chosen.path, worktree);
    assert_eq!(
        chosen.reason,
        RootReason::Marker {
            file: "Cargo.toml",
            bounded_by: Some(worktree.clone()),
        },
        "a `.git` file bounds the worktree's span as a `.git` directory would"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_directory_of_scripts_is_its_own_root() {
    let root = tree("scripts", &["bin"], &["bin/deploy.sh", "bin/backup.sh"]);

    let chosen = Ancestry::gather(&root.join("bin")).root();
    assert_eq!(chosen.path, root.join("bin"));
    assert_eq!(
        chosen.reason,
        RootReason::WorkingDirectory { vcs_root: None },
        "no marker and no repository: the working directory is the floor"
    );
    assert!(
        chosen
            .explain()
            .contains("no project marker and no repository")
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn a_marker_without_a_repository_still_answers() {
    // The measured fixtures had no repository at all, and the servers still
    // found the workspace. An unbounded walk is bounded by ANCESTOR_LIMIT
    // rather than refusing.
    let root = tree(
        "no-vcs",
        &["crates/alpha/src"],
        &["Cargo.toml", "crates/alpha/Cargo.toml"],
    );

    let chosen = Ancestry::gather(&root.join("crates/alpha/src")).root();
    assert_eq!(chosen.path, root);
    assert_eq!(
        chosen.reason,
        RootReason::Marker {
            file: "Cargo.toml",
            bounded_by: None,
        }
    );
    assert!(chosen.explain().contains("no repository found"));

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn markers_are_outermost_first_and_languages_follow_them() {
    // A JS repository with a Go service inside it: both markers are
    // ancestors, and the outermost is the key. The order is stated, not
    // incidental — a plan whose order varies cannot be reproduced.
    let root = tree(
        "mixed",
        &[".git", "services/api/internal"],
        &["package.json", "services/api/go.mod"],
    );

    let ancestry = Ancestry::gather(&root.join("services/api/internal"));
    let files: Vec<&str> = ancestry.markers.iter().map(|m| m.file).collect();
    assert_eq!(files, vec!["package.json", "go.mod"], "outermost first");
    assert_eq!(ancestry.root().path, root);
    assert_eq!(
        ancestry.languages(),
        vec!["javascript", "typescript", "go"],
        "languages follow marker order and are deduplicated"
    );

    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn gathering_the_same_tree_twice_gives_the_same_value() {
    let root = tree(
        "deterministic",
        &[".git", "a/b/c"],
        &["Cargo.toml", "package.json", "a/Cargo.toml"],
    );

    let once = Ancestry::gather(&root.join("a/b/c"));
    let twice = Ancestry::gather(&root.join("a/b/c"));
    assert_eq!(once, twice);
    // And the order within one directory is the table's, not the
    // filesystem's.
    let files: Vec<&str> = once.markers.iter().map(|m| m.file).collect();
    assert_eq!(files, vec!["Cargo.toml", "package.json", "Cargo.toml"]);

    let _ = std::fs::remove_dir_all(&root);
}
