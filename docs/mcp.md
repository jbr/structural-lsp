# structural MCP server (slice 1: `show`)

One stdio MCP server exposing a single tool, `show`, that resolves a bare
symbol name against rust-analyzer and returns the definition's decorated
source. No commits have been made in this slice.

## Running

```sh
# Serve on stdio (MCP clients spawn this). Root defaults to the cwd.
structural serve

# Fixed root, canonicalized at start.
structural serve --root /path/to/rust/project
```

- The root is fixed at server start; the tool's `path` argument is only a
  scope inside it (canonicalized; symlink escapes are rejected).
- rust-analyzer is spawned eagerly at server start. If it cannot start, the
  server still comes up, answers `initialize`/`tools/list`, and every call
  returns an actionable `isError` naming the fix
  (`rustup component add rust-analyzer`) — no panic, no stdout contamination.
- Readiness: the first call waits, bounded (30s), for rust-analyzer's
  `experimental/serverStatus` quiescence. Readiness follows the latest status
  (a `false` ends the quiescent epoch); results carry readiness evidence.

## Tool: `show`

```json
{ "name": "show", "arguments": { "symbol": "Config", "path": "src/lib.rs" } }
```

- `symbol` (required): bare name, matched exactly. Qualified paths
  (`Config::new`), line:col, and anchor/focus input are rejected with guidance.
- `path` (optional): scope under the fixed root; narrows candidates only.
- Behavior: refresh all previously opened documents from disk *before*
  discovery; resolve via `workspace/symbol` (exact/fuzzy/ambiguous — only one
  exact match resolves); open the definition target and re-query once, bounded,
  when its on-disk state was unknown; render the tree-sitter-shaped item
  (outer docs + attributes) with a 12 KiB marked budget; multiple definition
  locations are listed, never arbitrarily picked; files that keep changing
  yield a retry/status answer, never a stale body.
- Lifecycle: every re-index settle returns the readiness — a `Pending` result
  makes the tool answer with status and issue no semantic query (status-only
  policy). Deleting a previously opened document discards its snapshot and
  sends `textDocument/didClose` (the server never keeps a ghost open
  document); recreating the file produces a fresh `didOpen`. Readiness
  decisions drain the already-queued messages first, so a status that arrived
  while the server was idle is seen before any quiescence is claimed.
- The only view is `definition` (the schema has no `view` field); nothing else
  is advertised.

## Environment / knobs

- `STRUCTURAL_WIRE=1` — log the full LSP wire to `target/structural-wire.log`
  (each pump). Test hooks: `set_debug` exists on the client for tests.
- Event log (diagnostics) is capped at 2000 entries and holds incoming
  notifications/requests plus outgoing client notifications (didOpen/
  didChange/didClose, in send order); readiness lives in protocol state, not
  the log. The reader channel itself is not bounded.
- `cargo run -- spike --out target/spike` — the standalone probe against
  rust-analyzer (and pyright) retained from the spike phase.

## Tests

```sh
cargo test                                # unit + mock + non-gated smoke
cargo test --test mcp_smoke -- --ignored  # + 4 rust-analyzer e2e smokes
```

`tests/mcp_smoke.rs`: missing-rust-analyzer actionable error (non-gated);
in-process `show` (positive, scoped, ambiguous, qualified rejection, escape
rejection); real stdio initialize/`tools/list`/`tools/call` round trip
(incl. `notifications/initialized` sequencing, ping); document-refresh
(body changed on disk is served fresh); freshness across on-disk edits
(insert-before-declaration shift, rename/removal, second-module edit).

## Layout

- `src/lsp/` — promoted transport + kernel: framing, jsonrpc, handlers,
  positions (UTF-16 incl. CRLF/CR edge cases), client, session,
  definition (resolution + normalization), shape (tree-sitter).
- `src/state.rs`, `src/tools.rs`, `src/tools/show.rs` — MCP surface.
- `src/main.rs` — `serve` (root pre-scan) + `spike`; binary consumes the
  library modules, so tests compile once.
