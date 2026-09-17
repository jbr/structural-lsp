//! The fixture trees, as a specification rather than as resident source.
//!
//! A fixture is a small project in some language that the tests and the
//! conformance probe (`process/testing.md`) ask questions about. Every one
//! of them is **materialized into a scratch directory** before it is used;
//! none is read in place. That is what lets them live here as text and not
//! as a tree: `fixtures/` holds one `.in` file per fixture file, and the
//! suffix is what keeps them out of this project's own semantic tree.
//!
//! The suffix is not cosmetic. structural discovers a workspace's servers
//! from what the tree contains, and warms every server it selects, so a
//! resident Python tree here means a Python server on every session in this
//! repository — and a Kotlin one means a Kotlin server. Naming the file
//! `helper.py.in` means no language claims it, no extension table matches
//! it, and running structural on structural sees one language. It also
//! keeps a fixture's symbols out of this repository's own answers, which
//! has gone wrong before: a Python `resolve` under `reference/` is why the
//! `identity` acceptance is a standing failure
//! (`backlog/identity-test-uses-the-repo.md`).
//!
//! A materialized tree is reproducible from checked-in text, so a scorecard
//! that names a [`Fixture::revision`] names the tree it measured even
//! though that tree is not in the repository. To get one to work in by
//! hand, write it somewhere outside this repository:
//!
//! ```sh
//! cargo run --bin fixtures -- ../structural-fixtures
//! ```

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io;
use std::path::{Path, PathBuf};

/// One file of a fixture: where it goes, relative to the tree's root, and
/// what is in it.
pub struct File {
    pub path: &'static str,
    pub text: &'static str,
}

/// A fixture tree for one language.
pub struct Fixture {
    /// The language it is written in, and the directory name it is
    /// materialized under when several share a root.
    pub language: &'static str,
    pub files: &'static [File],
}

/// Shorthand for a file whose text is the checked-in `.in` beside it.
macro_rules! file {
    ($path:literal) => {
        File {
            path: $path,
            text: include_str!(concat!("../fixtures/", $path, ".in")),
        }
    };
}

/// Every fixture, and the whole of what a language needs to be probed:
/// something with a method to qualify (`Config.doubled`), a free function
/// called from a **second file** so cross-file references have somewhere to
/// be found (`choose`), and an identity function whose name is common
/// enough to collide.
pub const FIXTURES: &[Fixture] = &[
    Fixture {
        language: "python",
        files: &[file!("python/helper.py"), file!("python/main.py")],
    },
    Fixture {
        language: "rust",
        files: &[file!("rust/Cargo.toml"), file!("rust/src/lib.rs")],
    },
    Fixture {
        language: "go",
        files: &[
            file!("go/go.mod"),
            file!("go/greet.go"),
            file!("go/main.go"),
            file!("go/greet_test.go"),
        ],
    },
];

/// The fixture for a language, by the name [`Fixture::language`] carries.
pub fn get(language: &str) -> Option<&'static Fixture> {
    FIXTURES.iter().find(|fixture| fixture.language == language)
}

impl Fixture {
    /// Write the tree under `root`, creating parent directories. The path
    /// each file carries is relative, so `root` is the tree's own root —
    /// `root.join(fixture.language)` is the caller's choice, not this
    /// function's, because a test that wants two languages under one root
    /// needs them nested and a probe measuring one language does not.
    pub fn materialize(&self, root: &Path) -> io::Result<()> {
        for file in self.files {
            let target = root.join(file.path_within());
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(target, file.text)?;
        }
        Ok(())
    }

    /// A token over the fixture's file names and text.
    ///
    /// Recorded beside a server's version wherever a measurement is
    /// reported: a score is one observation on a server *and a tree*, and
    /// the tree is not in the repository to point at (`process/testing.md`).
    pub fn revision(&self) -> String {
        let mut hasher = DefaultHasher::new();
        for file in self.files {
            file.path.hash(&mut hasher);
            file.text.hash(&mut hasher);
        }
        format!("{:08x}", hasher.finish() as u32)
    }
}

impl File {
    /// The path within the tree, with the fixture's own directory stripped:
    /// `rust/src/lib.rs` is written as `src/lib.rs`.
    fn path_within(&self) -> &str {
        self.path
            .split_once('/')
            .map_or(self.path, |(_, rest)| rest)
    }
}

/// Materialize every fixture under `root`, each in a directory named for
/// its language, and return where each went.
pub fn materialize_all(root: &Path) -> io::Result<Vec<PathBuf>> {
    let mut written = Vec::with_capacity(FIXTURES.len());
    for fixture in FIXTURES {
        let target = root.join(fixture.language);
        fixture.materialize(&target)?;
        written.push(target);
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_fixture_writes_its_tree_with_the_language_directory_stripped() {
        let root = std::env::temp_dir().join(format!("structural-fixture-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let rust = get("rust").expect("the rust fixture");
        rust.materialize(&root).unwrap();

        // `rust/src/lib.rs` in the spec is `src/lib.rs` in the tree: the
        // caller decides whether the language nests, so a fixture used
        // alone is not buried under its own name.
        assert!(root.join("Cargo.toml").is_file());
        assert!(root.join("src/lib.rs").is_file());
        assert!(!root.join("rust").exists());

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_revision_follows_the_text_and_not_the_run() {
        let python = get("python").expect("the python fixture");
        assert_eq!(python.revision(), python.revision());
        assert_ne!(python.revision(), get("go").unwrap().revision());
    }

    #[test]
    fn every_fixture_declares_files_under_its_own_directory() {
        for fixture in FIXTURES {
            assert!(
                !fixture.files.is_empty(),
                "{} has no files",
                fixture.language
            );
            for file in fixture.files {
                assert!(
                    file.path.starts_with(&format!("{}/", fixture.language)),
                    "{} is not under {}/",
                    file.path,
                    fixture.language
                );
            }
        }
    }
}
