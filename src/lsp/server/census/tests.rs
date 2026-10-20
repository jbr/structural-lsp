use super::*;
use crate::lsp::adapter::rust_analyzer::RustAnalyzer;
use std::fs::File;

/// A scratch tree, removed when the test ends.
struct Tree(PathBuf);

impl Tree {
    fn new(tag: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("structural-census-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        Self(std::fs::canonicalize(root).unwrap())
    }

    /// Write `relative`, dated well before any census this test takes, so
    /// only a real change can move its stamp.
    fn old(&self, relative: &str, text: &str) -> PathBuf {
        let path = self.0.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
        let long_ago = SystemTime::now() - Duration::from_secs(60);
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(long_ago)
            .unwrap();
        path
    }

    fn census(&self) -> Census {
        Census::take(&self.0, &RustAnalyzer)
    }
}

impl Drop for Tree {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn an_untouched_tree_has_no_changes() {
    let tree = Tree::new("untouched");
    tree.old("src/lib.rs", "pub fn a() {}\n");
    let before = tree.census();
    assert_eq!(before.changes_until(&tree.census()), Changes::default());
}

#[test]
fn created_written_and_removed_served_files_are_changes() {
    let tree = Tree::new("changes");
    let kept = tree.old("src/kept.rs", "pub fn kept() {}\n");
    let written = tree.old("src/written.rs", "pub fn a() {}\n");
    let removed = tree.old("src/removed.rs", "pub fn gone() {}\n");
    let before = tree.census();

    std::fs::write(&written, "pub fn a() {}\npub fn b() {}\n").unwrap();
    std::fs::remove_file(&removed).unwrap();
    let created = tree.0.join("src/created.rs");
    std::fs::write(&created, "pub fn new() {}\n").unwrap();

    let changes = before.changes_until(&tree.census());
    let mut changed = vec![created, written];
    changed.sort();
    assert_eq!(changes.changed, changed);
    assert_eq!(changes.deleted, vec![removed]);
    assert!(!changes.changed.contains(&kept));
}

#[test]
fn unserved_files_and_skipped_directories_are_not_counted() {
    let tree = Tree::new("unserved");
    let before = tree.census();
    std::fs::write(tree.0.join("notes.txt"), "not rust\n").unwrap();
    std::fs::create_dir_all(tree.0.join("target/debug")).unwrap();
    std::fs::write(tree.0.join("target/debug/build.rs"), "fn main() {}\n").unwrap();
    std::fs::create_dir_all(tree.0.join(".hidden")).unwrap();
    std::fs::write(tree.0.join(".hidden/tool.rs"), "fn main() {}\n").unwrap();
    assert_eq!(before.changes_until(&tree.census()), Changes::default());
}

#[test]
fn a_stamp_too_close_to_its_census_is_not_trusted() {
    // Written just before the census, so a second write of the same
    // length within the filesystem's tick would leave the stamp as it
    // was: the next census pushes it rather than trust it.
    let tree = Tree::new("racy");
    let path = tree.0.join("src/lib.rs");
    std::fs::write(&path, "pub fn a() {}\n").unwrap();
    let before = tree.census();
    assert_eq!(before.changes_until(&tree.census()).changed, vec![path]);
}
