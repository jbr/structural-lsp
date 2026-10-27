//! Write the fixture trees somewhere, usually outside this repository.
//!
//! ```sh
//! cargo run --bin fixtures -- ../structural-fixtures
//! ```
//!
//! The tests do not need this: each materializes what it needs into its own
//! scratch directory, so `cargo test` depends on nothing on disk. This is
//! for working a tree by hand — pointing an editor or structural itself at
//! a real project in a language this repository does not contain
//! (`src/fixtures.rs`).

use std::path::PathBuf;

fn main() -> std::io::Result<()> {
    let Some(target) = std::env::args().nth(1).map(PathBuf::from) else {
        eprintln!("usage: fixtures <directory>");
        std::process::exit(2);
    };
    for written in structural_lsp::fixtures::materialize_all(&target)? {
        let fixture = structural_lsp::fixtures::FIXTURES
            .iter()
            .find(|fixture| written.ends_with(fixture.language))
            .expect("written from the same list");
        println!("{} · {}", written.display(), fixture.revision());
    }
    Ok(())
}
