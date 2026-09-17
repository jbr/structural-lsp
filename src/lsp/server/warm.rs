//! Choosing one file to open so a server's symbol index is populated.
//!
//! Seven of the nineteen servers measured in
//! `reference/lsp-server-conventions.md` answer `workspace/symbol` with
//! nothing until some document is open, and answer the identical query once
//! one is. A bare name is the most common query shape there is, so on a
//! fresh session those servers made structural say `no symbol named 'x'`
//! about a symbol plainly in the tree.
//!
//! **Any** file does it. Measured through the tools: opening `main.py`
//! makes a symbol defined in `helper.py` — never opened — resolve against
//! pyright, and clangd reaches `helper.hpp` through the include in
//! `main.cpp`. So this is not a guess about which file holds the answer,
//! and the cost is one file read per server rather than one per file in the
//! tree.
//!
//! The walk is **bounded and deterministic**: entries are sorted, build and
//! metadata directories are skipped, and it gives up after
//! [`ENTRY_BUDGET`] entries rather than descend a tree of unknown size
//! during startup. Finding nothing is normal — a workspace may hold no file
//! of that server's language — and warming is skipped without complaint.

use crate::lsp::adapter::{Adapter, language_for};
use std::path::{Path, PathBuf};

/// Directory entries examined before the search gives up.
///
/// A server that starts while a huge tree is walked is a server that is not
/// answering, so this is a bound on startup latency rather than on
/// correctness: exhausting it skips warming, and the first file-scoped
/// query warms the server anyway.
const ENTRY_BUDGET: usize = 4096;

/// Directory names never descended into: build output and metadata, where a
/// generated file would warm the index with symbols the caller did not
/// write.
const SKIP: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    "target",
    "node_modules",
    "build",
    "dist",
    ".venv",
    "venv",
    "__pycache__",
    ".mypy_cache",
    ".tox",
    "vendor",
    ".build",
];

/// The first file under `root` this adapter serves, in a stable order.
pub fn first_served_file(root: &Path, adapter: &dyn Adapter) -> Option<PathBuf> {
    let mut budget = ENTRY_BUDGET;
    search(root, adapter, &mut budget)
}

fn search(directory: &Path, adapter: &dyn Adapter, budget: &mut usize) -> Option<PathBuf> {
    let mut entries: Vec<_> = std::fs::read_dir(directory).ok()?.flatten().collect();
    // Sorted, so the same tree warms with the same file every time: a
    // readiness step that varies between runs is one that cannot be
    // reproduced from a report.
    entries.sort_by_key(std::fs::DirEntry::file_name);

    let mut directories = Vec::new();
    for entry in entries {
        if *budget == 0 {
            return None;
        }
        *budget -= 1;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_dir() {
            if !name.starts_with('.') && !SKIP.contains(&name.as_ref()) {
                directories.push(entry.path());
            }
            continue;
        }
        // Symlinks are not followed: a link out of the tree would open a
        // document the workspace does not own.
        if file_type.is_file() && language_for(&adapter.languages(), &entry.path()).is_some() {
            return Some(entry.path());
        }
    }
    // Files in this directory before any subdirectory, so a shallow file
    // wins over a deep one and the budget is spent near the root.
    directories
        .into_iter()
        .find_map(|directory| search(&directory, adapter, budget))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lsp::adapter::Language;

    struct Python;
    impl Adapter for Python {
        fn name(&self) -> &str {
            "python"
        }
        fn command(&self) -> Vec<String> {
            vec!["python".to_string()]
        }
        fn languages(&self) -> Vec<Language> {
            vec![Language::new("python", &["py"])]
        }
    }

    fn tree(tag: &str, files: &[&str]) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("structural-warm-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for file in files {
            let path = root.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "x = 1\n").unwrap();
        }
        root
    }

    #[test]
    fn the_first_served_file_is_stable_and_shallow() {
        let root = tree("order", &["deep/nested/z.py", "b.py", "a.py"]);
        // Sorted within a directory, and files before subdirectories.
        assert_eq!(
            first_served_file(&root, &Python),
            Some(root.join("a.py")),
            "the shallowest file, first by name"
        );
        assert_eq!(
            first_served_file(&root, &Python),
            first_served_file(&root, &Python)
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_language_the_adapter_does_not_serve_warms_nothing() {
        let root = tree("other", &["notes.md", "main.rs"]);
        assert_eq!(first_served_file(&root, &Python), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn build_output_and_metadata_are_not_descended() {
        // The only `.py` files are under skipped directories, so a warm
        // would otherwise open generated or vendored source.
        let root = tree(
            "skip",
            &[
                "target/generated.py",
                "node_modules/pkg/index.py",
                ".git/hooks/hook.py",
                ".venv/lib/site.py",
            ],
        );
        assert_eq!(first_served_file(&root, &Python), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_file_below_a_skipped_sibling_is_still_found() {
        let root = tree("mixed", &["target/generated.py", "src/app.py"]);
        assert_eq!(
            first_served_file(&root, &Python),
            Some(root.join("src/app.py"))
        );
        let _ = std::fs::remove_dir_all(&root);
    }
}
