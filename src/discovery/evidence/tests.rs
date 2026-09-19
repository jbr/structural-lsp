//! Sources read from constructed trees. Each case names a rule the scan
//! was given deliberately, so changing the rule fails a named test.

use super::*;
use std::path::PathBuf;

/// A scratch tree: directories to create, then files to write (empty).
fn tree(name: &str, directories: &[&str], files: &[&str]) -> PathBuf {
    let root =
        std::env::temp_dir().join(format!("structural-evidence-{}-{name}", std::process::id()));
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
fn extensions_are_counted_across_the_tree() {
    let root = tree(
        "counted",
        &["src", "tests"],
        &[
            "src/main.rs",
            "src/lib.rs",
            "tests/it.rs",
            "README.md",
            "Cargo.toml",
        ],
    );

    let sources = Sources::scan(&root);
    assert_eq!(sources.count("rs"), 3);
    assert_eq!(sources.count("md"), 1);
    assert_eq!(sources.count("toml"), 1);
    assert_eq!(sources.files_seen, 5);
    assert!(!sources.truncated);
}

#[test]
fn build_output_and_dependencies_do_not_describe_the_project() {
    // The case the skip list exists for: one JavaScript dependency tree
    // holding more files than the project has of its own language would
    // otherwise make every tree look like a JavaScript project.
    let root = tree(
        "skipped",
        &["src", "node_modules/left-pad", "target/debug", ".git"],
        &[
            "src/main.rs",
            "node_modules/left-pad/index.js",
            "node_modules/left-pad/a.js",
            "node_modules/left-pad/b.js",
            "target/debug/build.rs",
            ".git/config",
        ],
    );

    let sources = Sources::scan(&root);
    assert_eq!(sources.count("rs"), 1, "target/ is not the project's source");
    assert_eq!(sources.count("js"), 0, "node_modules is not evidence of JavaScript");
    assert_eq!(sources.files_seen, 1);
}

#[test]
fn a_dotfile_is_a_name_not_an_extension() {
    // `.gitignore` is a file called `.gitignore`, not a file with
    // extension `gitignore`; counting it as one invents a language.
    let root = tree("dotfiles", &[], &[".gitignore", "Makefile", "a.rs"]);

    let sources = Sources::scan(&root);
    assert_eq!(sources.count("gitignore"), 0);
    assert_eq!(sources.by_extension.keys().collect::<Vec<_>>(), vec!["rs"]);
    assert_eq!(sources.files_seen, 3, "they are seen, they just name nothing");
}

#[test]
fn extensions_are_lowercased_so_one_language_is_one_key() {
    let root = tree("case", &[], &["A.RS", "b.Rs", "c.rs"]);

    let sources = Sources::scan(&root);
    assert_eq!(sources.count("rs"), 3);
}

#[test]
fn the_scan_says_when_it_stopped_early() {
    // Truncation has to be visible: an extension absent from a partial
    // scan is not evidence that the tree does not contain it.
    let files: Vec<String> = (0..FILE_BUDGET + 10).map(|n| format!("f{n}.rs")).collect();
    let borrowed: Vec<&str> = files.iter().map(String::as_str).collect();
    let root = tree("budget", &[], &borrowed);

    let sources = Sources::scan(&root);
    assert!(sources.truncated);
    assert_eq!(sources.files_seen, FILE_BUDGET);
}

#[test]
fn the_scan_starts_at_the_root_not_the_working_directory() {
    // An agent in one member of a cargo workspace: the sibling member's
    // sources are still this project's, because the root is what the
    // workspace is keyed by and what a server is told.
    let root = tree(
        "from-root",
        &[".git", "crates/alpha/src", "crates/beta/src"],
        &[
            "Cargo.toml",
            "crates/alpha/Cargo.toml",
            "crates/alpha/src/lib.rs",
            "crates/beta/Cargo.toml",
            "crates/beta/src/lib.rs",
            "crates/beta/src/helper.rs",
        ],
    );

    let evidence = Evidence::gather(&root.join("crates/alpha/src"));
    assert_eq!(evidence.ancestry.root().path, root);
    assert_eq!(
        evidence.sources.count("rs"),
        3,
        "beta's sources are part of the same project"
    );
}

#[test]
fn a_tree_with_no_marker_is_scanned_from_the_working_directory() {
    let root = tree("no-marker", &["scripts"], &["scripts/deploy.sh", "notes.txt"]);

    let evidence = Evidence::gather(&root.join("scripts"));
    assert_eq!(evidence.ancestry.root().path, root.join("scripts"));
    assert_eq!(evidence.sources.count("sh"), 1);
    assert_eq!(
        evidence.sources.count("txt"),
        0,
        "the root is the working directory, so the parent is not scanned"
    );
}
