//! The `structural` binary: an MCP server over stdio (`structural serve`) or a
//! tool run directly from argv (`structural show choose`). mcplease owns the
//! argv parsing for both.
//!
//! The workspace root is the current directory, or `STRUCTURAL_ROOT` when
//! set: the MCP front end's default workspace context, acquired through the
//! same host an embedded consumer uses. Every configured language server
//! starts before either path runs, so indexing begins immediately.

use anyhow::Context;
use std::path::PathBuf;
use structural::host::{AgentId, Host};
use structural::tools;

fn main() -> anyhow::Result<()> {
    let root = match std::env::var_os("STRUCTURAL_ROOT") {
        Some(root) => PathBuf::from(root),
        None => std::env::current_dir().context("resolving the workspace root")?,
    };
    let root = std::fs::canonicalize(&root)
        .with_context(|| format!("workspace root `{}` does not exist", root.display()))?;
    let host = Host::new();
    let mut session = host.agent_started(AgentId::mcp(), &root);
    let instructions = "\
show resolves a symbol to its definition's decorated source (or another \
`view` of it: outline, implementations, calls, type); uses lists the \
references to it, one source line per hit with its enclosing item, tests set \
apart heuristically; action previews and applies a rename, following \
identity across files. Target by name (`symbol`, bare or qualified by its \
container: `Config::new`) or by quoting source text (`anchor` from the file \
`path`, `focus` naming the token), which reaches locals and call sites. \
Candidates come from the language servers' fuzzy symbol index and are not \
proven exhaustive; only exact names resolve, and several distinct definitions \
are listed rather than picked. References follow symbol identity, not text. \
Every answer is current: owned files are re-synced from disk and the servers \
are waited for before each query. The workspace root is fixed at server \
start; `path` only scopes inside it.";
    let result = mcplease::run::<tools::Tools, _>(
        &mut session,
        mcplease::ServerConfig::new(mcplease::server_info!()).with_instructions(instructions),
    );
    // No language server outlives the front end that owns it.
    host.shutdown();
    result
}
