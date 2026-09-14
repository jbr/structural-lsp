# Slice 2 handoff: state after the 2026-09-14 afternoon sessions

Reconstructed from the efference logs of `warm-marsh` (gpt-6-astra supervisor),
`light-prism` (glm-5.3-flash implementer, ended after repeated OpenRouter
idle timeouts), and `fair-mesa` (deepseek-v4.1-flash, interrupted before it
did anything). `docs/slice-2.md` remains the specification; this file records
what was *decided* on top of it, so the next implementer does not repeat the
afternoon's design discussion.

## Where things stand

- Baseline `d2bcf32`: slice 1 committed, `cargo test` green (64 lib, 7 spike,
  1 smoke; 6 real-server tests `--ignored`). rust-analyzer and pyright are
  both on PATH.
- Working tree: `efference.toml` has a harness-added `../mcplease` read grant
  (keep, do not commit with implementation work). `src/lsp/adapter.rs` is the
  only code the afternoon produced: the `ReadinessPolicy` trait plus
  `StandardIdle`. Reviewed by the supervisor and accepted; not yet in
  `mod.rs`.
- Nothing else in `src/` changed. No handle, no tests, no migration.

## The one discovery that reshaped the plan

`mcplease::stdio::serve` reads, dispatches, and writes on one thread, so a
blocked `show` also blocks MCP `ping`. Item 1's "done when" (a 30 s readiness
wait does not delay ping) cannot be met by an LSP worker thread alone. That
work was split out:

- A downstream audit of mcplease consumers lives in
  `../mcplease/async-dispatch-audit.md` and
  `../mcplease/async-dispatch-audit-findings.md`. Conclusion: do the
  synchronous fix now; a sibling async API is a separate later proposal.
- Stage 1 is implemented, uncommitted, on mcplease branch `responsive-stdio`:
  a reader thread answers `ping`, `initialize`, `server/discover`,
  `tools/list`, and unknown methods inline; `tools/call` goes to the calling
  thread over a bounded queue (`ServerConfig::with_tool_queue_depth`, default
  16); overflow is refused with `-32000 "server busy: tool queue full"`;
  accepted calls drain on clean EOF; `serve_with` takes any reader/writer for
  tests. No new bounds on `State` or `Tools`.
- structural will consume it via `[patch.crates-io] mcplease = { path =
  "../mcplease" }` until it is released. Release is gated on structural's
  integration test: a real ping during a controlled blocked `show`, queue
  saturation, intact id correlation, and the direct CLI path unchanged.

## Checkpoint plan (supervisor's split)

Work in these checkpoints, each independently reviewed before the next:

1. **1A: the LSP handle and its tests** — done 2026-09-14 evening in
   `src/lsp/handle.rs` (Claude, directly). Existing `Client`/`Session`/`show`
   /spike untouched and still green. Two deviations from the proposal below:
   after a failure the owner thread lingers as a tombstone that refuses later
   calls with the failure reason (instead of exiting, which made every later
   error "connection closed"); and `window/workDoneProgress/create` opens a
   token as well as `$/progress begin`, so a created-but-not-begun token is
   outstanding work.
2. **1B: migrate `Session`, `show`, and the spike onto the handle** — done
   2026-09-14 evening. `Client` is deleted with its pump/drain/settle/epoch
   machinery and both retry loops in `show`. rust-analyzer's quiescence
   policy is `adapter::rust_analyzer::Quiescence`. `show` now syncs owned
   documents, waits for no outstanding work, queries, syncs the candidate's
   file, checks the symbol name sits at the reported range (an error if not),
   waits again, queries `definition`, syncs the target, checks its range the
   same way, then shapes. rust-analyzer's `workspace/symbol` and `definition`
   ranges are both the *name* range (verified from the spike's raw output),
   so the check is exact equality. All six real-server tests pass with no
   retries. Output wording is unchanged pending item 7; the readiness
   evidence lines are simply gone.
3. **MCP integration** against the patched mcplease — done 2026-09-14
   evening. `Cargo.toml` carries `[patch.crates-io] mcplease = { path =
   "../mcplease" }` (remove it, and bump the dependency, when mcplease is
   released). `tests/mcp_smoke/responsive.rs` proves the contract with a
   scripted `rust-analyzer` shim on `PATH` that stays not-ready until a
   release file appears: a `ping` sent behind a blocked `show` is answered
   first; sixteen further calls queue; four more are refused at once with
   `-32000 "server busy"`; after release the blocked call and the sixteen
   queued ones answer in arrival order. Not `--ignored`: it needs only
   `python3`. The real-server smokes pass unchanged on the patched crate.
4. **Item 3: adapter seam, second server, registry** — done 2026-09-14
   evening. `Session` became the generic `lsp::server::LspServer`; per-server
   knowledge is an `adapter::Adapter` (`rust_analyzer`, `pyright`, and
   `configured` for anything in `structural.toml`). rust-analyzer's adapter
   sets `all_symbols` and `excludeImports`, so the fixture's `Config` now
   resolves instead of listing its re-import (item 5's done-when, early),
   and offers utf-8 first; `positions` takes the negotiated `Encoding`.
   `State` is a registry: every configured server whose command spawns is
   started at once; `show` goes to the server for a file scope's language
   or to all servers, merging candidates, and every candidate line and
   result names its server. Two new gated tests: both built-ins from one
   scratch root (ambiguity across languages, directory and file scopes),
   and gopls through `structural.toml`. Python and Go bodies are the
   server's range only until item 6. Lesson from the first attempt: do not
   make `fixtures/` itself a root — rust-analyzer also searches parent
   directories for a `Cargo.toml` and loads this repository instead.
5. **Item 5: identity, not names** — done 2026-09-14 evening. Every exact
   `workspace/symbol` hit is synced, range-checked, and sent to
   `textDocument/definition`; `definition::group_by_target` folds the
   results into `Identity` values keyed by server and target (a hit with no
   definition is its own identity, listed not dropped). One identity is the
   resolution; several are the ambiguous listing with kinds, target paths
   and servers. `tests/mcp_smoke/identity.rs` runs against this repository:
   `show resolve` finds the function, `show Server` lists the spike's enum
   and the smoke tests' struct. (The plan's `Readiness` example is stale:
   that enum was deleted in 1B.) Item 4 was already in place from 1B.
6. **Item 6: extents from `documentSymbol`, tree-sitter for decoration** —
   done 2026-09-14 evening. `lsp::outline::innermost_at` reads either
   result shape and picks the innermost symbol containing the definition;
   `shape::extend_start` widens its start over Rust doc comments and
   attributes or Python decorators (`tree-sitter-python` added); a language
   without a grammar keeps the server's extent, which is already the whole
   item for gopls. `item_at` remains only as the Rust fallback when a server
   gives no extent, and the `body:` note appears only on a fallback path.
   Python and Go bodies are now complete functions in the gated tests.
7. **Item 7: output trimmed to what the agent needs** — done 2026-09-14
   evening. Happy path is `kind name · path:line`, the body, and `note:`
   lines only when something is degraded (truncation, fallback shaping,
   definition outside the root). Kind words come from the adapter
   (`fn`/`trait`/`mod` for rust-analyzer, `def` for pyright, neutral words
   otherwise). Listings are one line per entry; the ambiguous listing adds
   the outline's `detail` (rust-analyzer's signature) or the item's first
   line, and names servers only when entries span more than one. Exclusion
   counts appear as notes on listings only. The caveats live once in the
   tool description and the MCP `instructions`. The fixture `show choose`
   is exactly one header line plus the item text from the file; that is
   211 bytes because rust-analyzer's extent includes the fixture's `//
   @decl` comment, so the test asserts equality with the file rather than
   the plan's "under 200 bytes" (27 bytes of wrapper meets the "under 150
   beyond the body" target).
8. Item 8 of `docs/slice-2.md` — next: mcplease owns argv (`--root`
   becomes cwd plus `STRUCTURAL_ROOT`; the mounted `structural-dev` MCP
   command will need re-adding), the spike moves to `src/bin/spike`, the
   crate-wide `allow(dead_code)` goes, `show`'s `execute` under 80 lines.

## Settled design for 1A

These were argued out between supervisor and implementer and are not open.

**Threads per server.** Reader (child stdout → frames → classify → blocking
send into the owner inbox; backpressure lands on the child, never on API
callers). Owner (all protocol state; the only thread that touches pending
requests, progress tokens, the policy, the event log). Writer (owns child
stdin; the single writer, so wire order is enqueue order).

**Channels and bounds.** Owner inbox `sync_channel(1024)` carrying
`FromServer(Incoming) | Admit(Command)`. Writer queue `sync_channel(64)` of
frames, each optionally carrying an ack sender. Pending operations (requests +
ready waiters) capped at 256, open progress tokens at 4096, event log at
2000. All are `Settings` fields with those defaults; tests inject small ones.
Document that these bound counts, not bytes.

**Admission.** Handle callers use `try_send`; `Full` is an explicit local
overload error, never a block. The `Instant` is captured just before
`try_send` and carried in the command; that is the operation's start.
Requests are registered in the pending map *before* their frame is handed to
the writer (the owner is single-threaded, so no response can be processed
between the two). Writer `Full` on a caller's request or notify is an honest
refusal to that caller and the connection stays up. Writer `Full` on a
*required* server reply (e.g. the answer to `window/workDoneProgress/create`)
terminates the connection; a required frame is never silently dropped.

**Acks.** `notify` returns the writer's actual outcome, delivered per frame,
not enqueue success. On write failure the writer acks the failed job `Err`,
drains and fails every remaining queued job, then reports `WriteFailed` to
the owner. On teardown the owner sets a `terminating` flag before dropping
the writer's sender so queued jobs are acked `Err` rather than written.

**Backstop.** One timer, inactivity only. Per pending operation the deadline
is `max(admitted_at, last_incoming) + backstop`; any incoming message
(responses, notifications, server requests, even malformed frames) resets
`last_incoming`; EOF is death, not activity. Expiry is checked after every
processed message *and* on `recv_timeout` expiry, so continuous traffic
cannot starve it. A long-idle connection gets a fresh budget on its next
operation. The error names the server, elapsed inactivity, time since
admission, and the last message summary. Default 90 s.

**Readiness.** `ready = progress_tokens.is_empty() && policy.is_ready()`.
The transport tracks `window/workDoneProgress/create` and `$/progress`
begin/end by canonicalised token; the policy observes every notification and
may only make readiness *stricter*. No experimental or rust-analyzer string
in `handle.rs`.

**Shutdown.** `shutdown(self)` and `Drop` both: best-effort `try_send`
of `Command::Shutdown` (graceful fast path) *and* set a shared
`Arc<AtomicBool>` terminate flag (reliable path when the inbox is full). The
owner checks the flag on every wake; when idle with no deadlines it waits
with `recv_timeout(CONTROL_TICK)` (250 ms) so the flag is always noticed.
Both paths run the same sequence: bounded graceful attempt (shutdown request
+ exit notify, `shutdown_grace` default 5 s), then kill and reap the child
**before** waiting on the writer, then drop the writer sender, then bounded
joins (poll `is_finished` up to 2 s, then detach; the existing
`Client::cleanup` strand guard has this shape). Forced failures (EOF, write
failure, required-reply overflow) skip the graceful attempt. All pending
requests and ready waiters are failed with the reason. The control tick is
lifecycle machinery, not a settle pump; readiness waits stay message-driven.

**Snapshots.** Routine metadata (server info, capabilities, encoding, ready
flag, last-message summary) does not clone the event log; event capture is a
separate diagnostic command.

**Public surface (proposed).**

```rust
pub struct Settings { inbound_cap, writer_cap, pending_cap, progress_token_cap,
                      event_cap, backstop: Duration, shutdown_grace: Duration }
impl ServerHandle {
    pub fn spawn(command, args) -> Result<Self>;             // Settings::default(), StandardIdle
    pub fn spawn_with(command, args, Settings, Box<dyn ReadinessPolicy>) -> Result<Self>;
    pub fn request(&self, method, params) -> Result<Value>;  // blocking; no timeout param
    pub fn notify(&self, method, params) -> Result<()>;      // resolved by the writer's ack
    pub fn wait_ready(&self, backstop: Duration) -> Result<()>;
    pub fn snapshot(&self) -> Result<Snapshot>;
    pub fn capture_events(&self) -> Result<Vec<Value>>;
    pub fn shutdown(self) -> Result<()>;
}
```

## 1A acceptance tests (all deterministic, Python mock in the `client.rs` style)

- Reverse-order correlation: two requests from two threads, mock answers the
  second first, both callers get their own result.
- 10 000 unsolicited notifications while **no** call is active; the mock then
  answers a marker request. Receiving that response proves every earlier
  frame from the same producer (the reader) was processed; assert the event
  log is at its cap and a follow-up request works. No cross-producer ordering
  claim.
- Progress: token begin → not ready; end → ready; a test policy that says
  `false` keeps it not-ready even with no tokens; a policy saying `true` does
  not override an open token.
- Backstop: silent mock trips a 1 s backstop with the server name and last
  message in the error; a mock emitting `$/progress` reports every 50 ms for
  2 s does *not* trip it and the wait completes; after a long idle a new
  request succeeds.
- EOF mid-request fails every waiter and later calls fail fast.
- Genuinely blocked writer: a 512 KiB notify to a mock that stops reading
  stdin; `shutdown()` must return within its bound (kill unblocks the write).
- Required-reply overflow: mock stops reading stdin and emits more server
  requests than `writer_cap`; the connection terminates coherently and a
  subsequent `shutdown()`/drop is prompt and panic-free.
- Saturation + shutdown: small `inbound_cap`, threads hammering `request`,
  `shutdown()` completes within ~1 tick + grace via the flag path.
- Existing `cargo test` stays green; no new `allow(dead_code)`; no sleeps,
  pumps, or retries outside mock timing fixtures and the join guards.

## Known gaps in the existing code that 1B must handle

- `Client::wire_debug_enabled` is private; the handle wants it `pub(crate)`.
- `handlers::server_request_reply` needs the workspace folders captured from
  the outgoing `initialize` params; the owner captures them at admission.
- `jsonrpc::response_result(response, has_result)` is the explicit-null aware
  path; keep using it rather than reading `result` directly.
- `tests/mcp_smoke.rs` `wait_for(id)` helpers already tolerate interleaved
  responses; keep that when the MCP ping-during-show test lands.

## Open questions for jbr (none block 1A)

- Backstop default 90 s, config file name `structural.toml`, and
  `show`-without-path across several servers: unchanged from `slice-2.md`.
- Whether to release mcplease as 0.4.3 or 0.5.0 once structural's integration
  test passes (no `State`/`Tools` bound changes, but reordered responses are
  observable).

## Observations from Claude Code's rust-analyzer plugin (2026-09-14)

jbr enabled the official `rust-analyzer-lsp` plugin during 1B so its
behaviour could be compared with this design. Not to copy; to learn from.

- **It served a stale answer.** `findReferences` on `Client::pump` returned
  "No references found. This may occur if the symbol has no usages, or if
  the LSP server has not fully indexed the workspace." About a minute later
  the identical call returned 12 references across 6 files. That is the
  exact failure `docs/slice-2.md`'s governing rule exists to prevent: the
  hedge in the message does not help an agent that has to decide something
  now. Blocking until the server has no outstanding work would have made the
  first answer correct.
- **Position-based only.** Every operation needs a file, 1-based line and
  column; the only name-based entry is `workspaceSymbol`, which returns
  locations, not bodies. An agent has to already know where a symbol is to
  ask about it, then read the file itself. `show`'s bare-name-to-decorated-
  body design is the direct alternative.
- **Diagnostics are pushed into the conversation.** After each edit a
  `new-diagnostics` block listed rustc errors with file and line, before any
  build was run. Genuinely useful when fresh; it also surfaced an error in a
  `core` file (`result.rs`) as if it were ours, which is noise. Worth
  studying before slice 3's diagnostics view: the value is real, the
  staleness risk is the same as above.
- **Reference output is compact**: `file: line:col` per hit, grouped by
  file, no context lines. Good token economy; but with no snippet an agent
  has to open every file to learn anything, so `uses` should carry one line
  of context per hit. jbr's reaction on seeing it: "that doesn't seem very
  useful at all."
- **Diagnostics arrive mid-edit.** During item 3 the pushed diagnostics
  flagged "file not found for module `pyright`" for a module written seconds
  later, and errors in files half-way through a multi-file change. Reading
  them cost more than it saved; the useful moment for diagnostics is after a
  logical unit of work, on request, not after every write.
