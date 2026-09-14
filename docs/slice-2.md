# Slice 2: generic kernel, correctness, two servers

Plan written 2026-09-14 after reviewing slice 1 (`docs/mcp.md`). Slice 1 works
and its transport kernel is sound. Slice 2 reshapes the layers above the
kernel so that the next tool (`uses`, slice 3) is written once against a
generic server interface rather than against rust-analyzer. `uses` is not in
this slice.

## Governing rule

**Correct and current information beats response time.** The calling agent
has no sense of wall-clock time; the human cares that the answer is right.
Concretely:

- Before any semantic query, wait until the server has *no outstanding
  work*: every `$/progress` token has ended and any server-specific ready
  signal agrees. Wait for unrelated work too (another file's re-index, a
  `cargo check`). Never answer from an index known to be settling.
- The only timer is a **backstop for a broken server**, and it measures
  *inactivity*: time since the server last sent any message. A server that is
  still reporting progress is busy, not broken. Default 90 s; configurable
  per server. When it trips, the tool answers with an error naming the
  server and the last thing it said.
- Freshness after an edit comes from **LSP message ordering**, not from
  timing. A server must apply `textDocument/didChange` before it handles any
  request that follows it on the wire. So "push disk state, then query" is
  fresh by construction. No settle pumps, no readiness epochs, no re-query
  loops.
- Any answer that might be stale is wrong, not fast. Prefer blocking for a
  minute over shipping a guess.

Why: Claude Code's first LSP integration optimised for latency, served stale
diagnostics, and sent agents on spurious bug hunts. That is the failure to
design against.

## Evidence gathered during the review

| observation | value |
|---|---|
| rust-analyzer quiescence, fixture crate (5 files) | 6 to 9 s |
| rust-analyzer quiescence, this repo | 15 to 28 s |
| rust-analyzer quiescence, efference (104 k lines), two consecutive runs | 422 s, then 429 s (this environment is not representative: unbuilt recent changes, different toolchain env; but the second run was no faster, so whatever dominates is not being cached here) |
| `show` happy-path output on the fixture | 4 body lines, 9 wrapper lines |
| disk-watch lag when the client owns nothing (16 trials) | 65 to 95 ms, but one run lost the file from the index for good |

Two `workspace/symbol` findings that change the resolution design:

1. **rust-analyzer searches types first and only falls back to all symbols
   when the type search is empty.** Query `resolve` returns only the struct
   `Resolved`; the function `resolve` is invisible. With
   `workspace.symbol.search.kind = "all_symbols"` in `initializationOptions`
   the function appears. Any function whose name fuzzy-matches some type is
   unreachable under the default.
2. **Imports and re-exports are reported as symbols.** `pub use super::Config`
   makes `Config` ambiguous. `workspace.symbol.search.excludeImports = true`
   collapses it to the one definition. Verified on the fixture.

Both are rust-analyzer configuration and belong in its adapter.

## Work items, in order

Each item lists what to build, what must be true when it is done, and the
test that proves it. Do them in this order; later items assume earlier ones.

### 1. Per-server thread and request channel

Replace the "pump the channel only while a tool call is running" model.

- Each `Client` moves onto its own thread that owns the child process, the
  reader, and all protocol state. It loops forever over incoming messages:
  answers server requests, records progress and readiness, stashes responses.
- The tool side holds a handle with two blocking operations:
  `request(method, params) -> Result<Value>` and
  `notify(method, params) -> Result<()>`. Requests carry a reply channel.
  Ordering on the wire is the order calls are made on the handle.
- The handle also exposes `wait_idle(backstop) -> Result<()>` (item 2) and a
  snapshot of server info and capabilities.
- Nothing in the tool layer ever pumps, drains, or sleeps.

Done when: a 30 s readiness wait on one server does not delay MCP `ping`;
a notification flood between tool calls is consumed as it arrives (the mock
server test sends 10 000 notifications while no call is active and the
process stays bounded).

### 2. Readiness as "no outstanding work"

- Track `window/workDoneProgress/create` and `$/progress` begin/end per
  token in the server thread. `idle` means the token set is empty.
- An `Adapter` hook (item 3) may add a stronger signal. rust-analyzer's is
  `experimental/serverStatus.quiescent`; readiness for that server is
  `idle && quiescent`. pyright has none; readiness is `idle`.
- `wait_idle` blocks until ready, and errors only when the inactivity
  backstop trips. The backstop resets on every message from the server.
- Delete `Readiness` epochs, `settle`, `ensure_ready`'s bound, `last_wait`,
  and both retry loops in `show`.

Done when: the freshness e2e test passes with the retry loops gone; a mock
server that opens a progress token and never closes it makes `show` fail
with the backstop error after the configured inactivity, and a mock that
keeps sending progress reports for longer than the backstop does *not* trip
it.

### 3. Adapter seam and second server

Split `Session` into a generic `LspServer` and an `Adapter` trait.

`LspServer` (generic): spawn, initialize with the *honest* capability set
(what this client actually handles), record the server's capabilities and
position encoding, document ownership (item 4), progress tracking, the
standard queries, shutdown.

`Adapter` (per known server), all with defaults:

- `command()` and default args; install hint for the error message.
- `languages()`: language ids and file extensions it serves.
- `initialization_options()` and extra `experimental` capabilities.
- `is_ready(&ServerSignals) -> bool` beyond idle.
- `symbol_query(&str) -> String` if the server needs query rewriting.
- `extra_views()`: empty in this slice; the hook for slice 3.
- Which initialize capability profile to send, so the pyright
  `workspace.workspaceFolders` interoperability problem from `docs/spike.md`
  is expressed as adapter data.

Concrete adapters:

- **rust-analyzer**: `initializationOptions` sets
  `workspace.symbol.search.kind = "all_symbols"`,
  `workspace.symbol.search.excludeImports = true`; advertises
  `serverStatusNotification`; ready is idle and quiescent. Offer
  `positionEncodings: ["utf-8", "utf-16"]` and use whichever it picks
  (`positions.rs` takes the encoding as a parameter).
- **pyright** (`pyright-langserver --stdio`): no extensions, no ready signal,
  the reduced capability profile the spike verified.
- **generic**: any user-configured `command` + `languages`, everything at
  defaults.

Configuration: an optional `structural.toml` at the workspace root, with
`[servers.<name>] command = [...], languages = [...], backstop_secs = N`.
Built-in adapters are on by default; a config entry with the same name
overrides its command or backstop.

`State` becomes a registry: language id → server handle, plus an extension →
language map. All configured servers whose command is on `PATH` spawn at
startup so indexing begins immediately. `show` with a `path` picks the server
by extension; without one it queries every running server and merges.

Done when: `show` resolves the fixture function in `fixtures/python` through
pyright and in `fixtures/rust` through rust-analyzer from one process; the
e2e for each runs under `--ignored`; no rust-analyzer string appears outside
its adapter.

### 4. Document ownership, simplified

Keep the slice 1 decision that the client owns files it has touched (the
disk-watch alternative was measured: fast, but it lost a file from the index
once in five runs, and it is not deterministic). Simplify the mechanism:

- `sync(path)`: read disk; `didOpen` on first touch, full-text `didChange`
  when different, nothing when equal. Return the text.
- Before each query, `sync` every owned file (they are the only files whose
  freshness the server delegates to us). Then query. No wait in between
  beyond `wait_idle`.
- After the query, assert the result against the text you read: the
  candidate's name must sit at the returned range. A mismatch is an error
  reported to the caller, not a retry.
- Do not send `didSave` in this slice. It triggers rust-analyzer's
  check-on-save, which is wanted only once a diagnostics view exists (slice
  3+). Leave a comment at the one place it will go.

Done when: `in_process_show_stays_fresh_across_on_disk_edits` passes without
the retry loops, and a delete-then-recreate still produces `didClose` then
`didOpen`.

### 5. Identity, not names

- Resolution: `workspace/symbol` (with the adapter's query rewriting)
  nominates; exact name matches are then each sent to `textDocument/definition`
  and **deduplicated by definition target**. One target is a resolution;
  several distinct targets are listed as ambiguous with their kinds and
  paths.
- The fuzzy-only and not-found paths stay as they are.

Done when: on this repo, `show resolve` returns the function in
`definition.rs`; on the fixture, `show Config` returns the struct; on this
repo, `show Readiness` lists the enum and the spike struct as two targets.

### 6. Shaping via `documentSymbol`, tree-sitter for decoration

- Item extent comes from `textDocument/documentSymbol` (hierarchical):
  the innermost symbol whose range contains the navigation point gives the
  item range and kind. Works for any server.
- tree-sitter extends the range backwards over leading doc comments and
  attributes, per language: Rust (`///`, `/** */`, `#[...]`), Python
  (decorators, the docstring is inside the body already). A language without
  a grammar gets the LSP range only.
- If `documentSymbol` is unsupported or empty, fall back to the tree-sitter
  item search (today's `item_at`) where a grammar exists, else the server's
  own range. Say which path produced the body in one short line, only when
  it was a fallback.

Done when: `show` returns decorated bodies from both servers; the unit tests
in `shape.rs` still pass against the tree-sitter path.

### 7. Output trimmed to what the agent needs

Every byte in the result is paid for on every call. Target: the happy path
carries under 150 bytes beyond the body. Format:

```text
fn choose · src/lib.rs:43
pub fn choose(mode: Mode) -> usize {
    match mode { Mode::Fast => 1, Mode::Safe => 2 }
}
```

Rules:

- One header line: kind, name, path:line. Path relative to root.
- Body.
- Notes only when something is degraded, one line each: truncated (with the
  cutoff), fallback shaping, definition outside the root, server not ready.
- No footer. No readiness evidence on success. No "not a completeness
  claim", no "fixed at server start", no "in this slice". The tool
  description and the MCP `instructions` string carry the caveats once.
- Ambiguous and fuzzy listings: one line per candidate, kind, path:line, and
  a one-line signature when `documentSymbol` gives one.

Done when: the smoke tests assert on content, not on wrapper phrases, and the
fixture `show choose` result is under 200 bytes.

### 8. CLI and crate hygiene

- Remove the argv pre-scan in `main.rs`. mcplease owns argv; `serve` and the
  direct tool path (`structural show choose`) both work. The workspace root
  is the current directory, overridable by `STRUCTURAL_ROOT`.
- Move the spike to `src/bin/spike.rs` (it keeps using the library kernel).
- Remove the crate-wide `#![allow(dead_code)]`; delete or use what it hid.
- `tools/show.rs` `execute` under 80 lines; rendering in its own module.

Done when: `cargo clippy --all-targets -- -D warnings` is clean and
`structural show choose` prints a result from the fixture directory.

## Not in this slice

- `uses`, `action`, `inspect`, `outline`, any new schema field.
- Anchor/focus or line:col targeting.
- Diagnostics, `didSave`, flycheck.
- Sans-IO protocol state machine. The per-server thread is the shape
  efference will embed; a sans-IO core can be extracted from it later.
- typescript-language-server.

## Anti-patterns (from the slice 1 review)

The implementer should refuse these even if they seem to satisfy a review
comment:

- Disclaimers in tool output. Say what happened; do not annotate it.
- Retry loops or bounded re-queries as a substitute for ordering.
- Sleeps, short pumps, or "let the server catch up" delays of any length.
- Timers that return a partial result when they expire. The only timer is
  the inactivity backstop, and it returns an error.
- Server-specific strings, methods, or capabilities outside an adapter.
- Per-item `#[allow(dead_code)]` to keep unused fields.

## Verification

```sh
cargo test                                   # unit + mock
cargo test -- --ignored                      # rust-analyzer and pyright e2e
cargo clippy --all-targets -- -D warnings
cd fixtures/rust && ../../target/debug/structural show choose
```

Manual, once: point `serve` at `../efference`, call `show Session`, and
confirm it blocks through the cold index (several minutes) and answers
rather than reporting "unavailable". Record the cold and warm times in this
file.

Recorded 2026-09-14 (slice 2 complete), `structural show Session` against a
scratch copy of efference with `backstop_secs = 900`:

| observation | value |
|---|---|
| cold, answered | 448 s: `ambiguous: 4 definitions of 'Session'` (two structs, two enum variants), each with its path and signature |
| silent stretches from rust-analyzer during that cold index | 196 s (3 s to 199 s) and 229 s (200 s to 429 s); every other gap under 20 s |
| with the default 90 s backstop, two runs | both failed at 99 s and 100 s with the inactivity error, last message a `$/progress` |
| warm | not measurable this way: the process exits after one call and rust-analyzer keeps no index across processes; a second cold run took the same time |

So the proposed 90 s default is wrong for rust-analyzer on a project this
size: it goes quiet for minutes while cargo runs build scripts and
proc-macros, with a progress token open the whole time. The timer needs a
liveness probe (a `$/`-prefixed request the server must answer with
`MethodNotFound`) and a higher per-adapter default; decision pending.

## Questions for jbr, only if they block

- Backstop default: 90 s inactivity is proposed. Raise or lower?
- Config file name and location (`structural.toml` at the root is proposed).
- Whether `show` without `path` should query all servers or require a
  language when more than one is running.
