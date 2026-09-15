//! The `structural` binary: an MCP server over stdio (`structural serve`) or a
//! tool run directly from argv (`structural show choose`). mcplease owns the
//! argv parsing for both.
//!
//! The workspace root is the current directory, or `STRUCTURAL_ROOT` when
//! set. Every configured language server starts before either path runs, so
//! indexing begins immediately.

use anyhow::Context;
use std::path::PathBuf;
use structural::{state, tools};

fn main() -> anyhow::Result<()> {
    let root = match std::env::var_os("STRUCTURAL_ROOT") {
        Some(root) => PathBuf::from(root),
        None => std::env::current_dir().context("resolving the workspace root")?,
    };
    let root = std::fs::canonicalize(&root)
        .with_context(|| format!("workspace root `{}` does not exist", root.display()))?;
    let mut state = state::State::new(root);
    let instructions = "\
show resolves a symbol to its definition's decorated source; uses lists the \
references to it, one source line per hit with its enclosing item, tests set \
apart heuristically. Target by name (`symbol`, bare or qualified by its \
container: `Config::new`) or by quoting source text (`anchor` from the file \
`path`, `focus` naming the token), which reaches locals and call sites. \
Candidates come from the language servers' fuzzy symbol index and are not \
proven exhaustive; only exact names resolve, and several distinct definitions \
are listed rather than picked. References follow symbol identity, not text. \
Every answer is current: owned files are re-synced from disk and the servers \
are waited for before each query. The workspace root is fixed at server \
start; `path` only scopes inside it.";
    mcplease::run::<tools::Tools, _>(
        &mut state,
        mcplease::ServerConfig::new(mcplease::server_info!()).with_instructions(instructions),
    )
}
