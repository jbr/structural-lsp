# Shutdown escalation: bounded scope, explicit force

Status: **proposal for operator review; not implemented**.

This supplements unit 2 of `lifecycle-and-discovery.md`. It does not replace
its discovery roadmap. Written after reviewing `lifecycle-handoff.md` and
tracing the current shutdown path.

## What we are optimizing for

Shutdown is best-effort graceful. The priorities are:

1. do not leave owned server processes running after controlled shutdown;
2. avoid forcibly interrupting filesystem operations when practical;
3. keep the implementation proportionate to a short phase of service life.

The operator's HTTP shutdown convention applies here: the first request is
soft; a second request escalates. The caller decides when grace has elapsed.
There is no new automatic host-level grace timer. Existing transport teardown
bounds are a separate mechanism and need not disappear.

An embedded caller can protect work at the **whole tool-call boundary** and
quiesce dispatch before shutting down. We do not need a filesystem transaction
system or a new guard around each individual write to implement this unit.
Safe Rust does not by itself guarantee liveness or child-process cleanup; the
ownership and escalation paths still need tests.

## Current evidence

- `Host::shutdown` in `src/host.rs` closes launch admission, stops registered
  starting children, waits on launch guards, then takes each workspace lock
  to remove running servers. Running servers are not retained in the starting
  child registry after publication.
- A tool holds that workspace lock while making blocking LSP calls. A server
  can answer liveness probes indefinitely without answering the original
  request. Thus the sweep can wait forever before reaching its server.
- `Host::shutdown` retains its host-map mutex guard throughout this operation.
  Concurrent calls are serialized today; a second call cannot act as force
  escalation. An earlier conversational claim that two shutdown sweeps could
  run concurrently was incorrect.
- `Stopper::stop` in `src/lsp/handle.rs` requests the existing graceful
  teardown. The owner already has a grace window and direct-child kill/reap
  fallback (`src/lsp/handle/owner/lifecycle.rs`). There is no distinct force
  capability in `Stopper` today.
- Launch election precedes acquisition of its swansong guard. Implementation
  must inspect this admission/registration boundary rather than assume that
  waiting on currently registered guards covers every elected launch.
- No process-group setup was found in `src`. Killing a direct child is not a
  guarantee that its Cargo/build-script or other descendants have stopped.
- Rename writes are client-side (`src/tools/action/rename.rs::apply`). They
  are sequential, not transactional, and some server notifications occur
  between file moves. `workspace/applyEdit` is declined by the client.
- `src/main.rs` calls shutdown after `mcplease::run` returns. Signal and EOF
  behavior inside mcplease must be inspected before designing CLI wiring.

These are code observations, not claims that the unit is already built.

## Intended contract

### Soft request

- Close admission to new tool calls and launches at the appropriate owning
  boundary. Document the embedded caller's dispatch/quiescence obligations.
- Allow admitted tool calls to complete; then request ordinary LSP shutdown
  and reclaim owned processes. A stuck call can keep this phase pending.
- Requesting shutdown must not itself block on a workspace lock, protocol I/O,
  or completion. Waiting is a separate operation so escalation remains usable.
- Retain swansong for panic-safe completion tracking where useful. Do not
  replace tested primitives with a bespoke completion counter.

### Force request

- Monotonically escalate the same shutdown, including while another thread is
  waiting for soft completion. Repeated requests are safe and idempotent.
- Reach **starting and running** owned servers without taking a workspace/query
  lock or a host-map lock held by the waiter.
- Abandon pending LSP work and terminate owned server processes; callers must
  receive an explicit failure rather than a successful partial answer.
- Force here means force the servers, not kill structural's tool thread.
  Structural remains alive to unwind calls, reap children, and report results.
- Do not promise that an arbitrary caller holding a public workspace guard
  forever can be made to release it. Process termination must remain possible
  independently; distinguish that from completion of all caller-held work.

### Filesystem safety

Soft shutdown protects the entire tool call, including rename writes. That
is enough for this unit. Force may interrupt server communication and can
leave an already-started multi-file action partly applied; do not claim rollback
or transactionality. Structural must not deliberately kill its own process
mid-write as its implementation of server force shutdown.

Language servers and their descendants can write caches, build artifacts, or
other files. We cannot detect every such critical section. Grace gives them a
chance to finish; explicit force accepts that remaining risk. Improving local
rename atomicity is separate work, not a prerequisite for shutdown escalation.

## Process ownership and abrupt death

Process-tree cleanup belongs in this unit's investigation and implementation,
not merely a claim inferred from `Child::kill`.

On the current Unix platforms, investigate placing each server in an owned
process group and terminating remaining group members on teardown, with the
necessary direct-child reaping. Cover descendants that retain inherited pipes.
Signal targeting and group lifetime must be reviewed for stale-ID hazards;
never risk signalling the harness's group. State the supported platform scope.
Descendants that deliberately detach require an explicit limitation, not an
unqualified promise about every possible server program.

**Process groups alone do not clean up after structural is SIGKILLed.** Likewise,
a parent harness crash does not necessarily kill structural: it may instead
close MCP stdin, leave inherited descriptors open elsewhere, or explicitly
terminate its children. Audit and test the real EOF/transport-loss path and
route orderly disconnects into cleanup.

Uncatchable death of structural needs an external supervisor or an OS-specific
parent-death mechanism. A portable supervisor is not silently included in this
unit. Report which abrupt-parent-loss scenarios work after the transport audit;
if that leaves the operator's main deployment unprotected, bring back that
specific gap before expanding scope. No in-process Rust destructor can make
SIGKILL cleanup reliable.

## Implementation sequence

1. Reproduce the running-query shutdown stall with the scripted mock: answer
   probes but withhold the query result. Use an outer test-process timeout.
2. Inspect mcplease signal/EOF behavior and the process-owner failure paths.
   Record the supported lifecycle boundaries and proposed public API names.
3. Implement nonblocking soft/force requests and separately waitable completion.
   Keep stop capabilities reachable for the full child lifetime, without a
   publication gap. Make admission plus completion registration coherent with
   shutdown. Keep blocking cleanup outside coordination locks.
4. Add owned-process-tree teardown and explicit force handling in the owner.
   Ensure spawn/thread-creation failures also reclaim children already started.
5. Wire CLI first/second signals, disconnect handling, and embedded API usage.
   Do not perform blocking teardown in a signal handler. Preserve existing
   convenience shutdown behavior where practical, with documented semantics.
6. Update stale lifecycle prose and independently review the interleavings.

The exact types are deliberately not prescribed here. A generation counter,
extra runtime, or replacement synchronization framework is not a prerequisite.
A mere `Starting` check is not a substitute for generation identity if future
stop/restart policy permits an old launch to overlap a new one.

## Done when: falsifiable checks

Use mock events, barriers, and recorded PIDs rather than timing sleeps to pick
interleavings. Outer timeouts turn deadlocks into failures, not product policy.

- A normal admitted call completes under soft shutdown; later calls are refused.
- A query that answers probes but never completes can be interrupted by force
  from another thread while soft shutdown is waiting. Its child is reaped and
  the waiting tool returns an error.
- Force during initialization and around publication leaves no live owned
  child or late publication after completed shutdown.
- A launch paused between election and completion registration cannot evade
  the shutdown completion contract.
- Repeated/concurrent requests share a monotonic shutdown state and completion
  outcome; no caller reports completed cleanup while owned cleanup is pending.
- A mock server that spawns a descendant, including one holding a pipe open,
  leaves neither process running after controlled force cleanup. Reaping
  obligations are stated correctly: direct children versus other descendants.
- Orderly MCP EOF cleans up; the supported signal sequence escalates while a
  call is blocked. Test the front end, not only Host methods.
- Soft shutdown during rename does not interrupt the admitted call. Forced
  interruption reports failure without promising that prior writes rolled back.
- Tests cover child cleanup on spawn/initialization failure and guard release
  on unwind where practical. Uncatchable process death is a documented limit.

Run `cargo test`, `cargo clippy --all-targets -- -D warnings`, and
`cargo doc --no-deps`; bound process-lifecycle integration tests externally.
Use a fresh reviewer to try to break admission, publication, and force-reachability
invariants. Passing happy-path tests is not evidence against a deadlock.

## Review gate

Operator reviews this scope before commit or implementation. In particular:
this is externally escalatable, best-effort graceful shutdown with process
ownership cleanup—not crash-proof editing, a portable supervisor, or a broader
redesign of server discovery and residency.

## Implementation record (2026-09-16)

Implemented after approval. Host/transport landed in `cadd47c`; frontend and
regression coverage follow in the implementation commit accompanying this note.

- `Host::request_shutdown()` closes admission without waiting;
  `force_shutdown()` interrupts starting and running server work independently
  of workspace locks; `wait_shutdown()` waits for admitted work and cleanup.
  `shutdown()` remains the convenience soft-request-and-wait operation.
- `Session::acquire()` now returns a fallible whole-operation guard. Tool
  admission and launch completion registration precede work under the same
  coordination lock used to close admission. Raw `Session::workspace()` remains
  a low-level escape hatch: its caller must release the guard before waiting.
- Stop capabilities stay reachable through publication and teardown.
  Transport force bypasses remaining protocol grace. The owner is joined, not
  detached on a timer; pipe-worker joins retain their existing bounds.
- On Unix, each server owns a process group. Cleanup signals the group before
  reaping its leader, keeping the group identifier pinned until signalling is
  finished. Deliberately detached descendants are outside this guarantee;
  non-Unix cleanup still targets the direct child. Grandchildren are terminated,
  not reaped by structural unless they are its children.
- Unix CLI signals are registered before eager startup. First signal requests
  soft shutdown; subsequent signals force. A separate cleanup waiter cancels
  serve input after admitted work completes. The watcher is explicitly closed
  before joining. No signal path calls `process::exit`.
- Real input EOF forces server cleanup in the reader, not after dispatch
  returns: dispatch might still be blocked on the server. Synthetic input
  cancellation after soft cleanup does not escalate. One-shot CLI dispatch is
  preserved. Signal support is Unix-specific; uncatchable structural death
  still needs external supervision.

The initial frontend implementation was faulty and is not the accepted design:
EOF left main joining a watcher blocked forever in `read`. A captured runtime
stack isolated that wait after all LSP threads had already exited. The exact
regression initially failed in 16.85 seconds; after repair it passed in 0.12
seconds in the final verification. This is why the frontend is tested as a real
subprocess, not inferred correct from the host tests.

Verification passed on the final implementation tree: `cargo test`,
`cargo clippy --all-targets -- -D warnings`, and `cargo doc --no-deps`, with
external TERM/KILL deadlines and unfiltered output. Tests include the original
EOF regression, explicit failure of a force-interrupted call, first-signal idle
exit, pre-initialization signals, admitted-call soft completion and response
drain, CLI help, process-group descendants, thread-start failures, and host
admission/force reachability. Real-server ignored tests were not run.

Remaining boundaries: no crash-proof multi-file edit transaction; no protection
against a caller retaining a raw workspace guard indefinitely; no portable
supervisor; no guarantee against an embedding application reaping structural's
children independently. The older lifecycle/handoff documents describe the
previous four-pass shutdown; this record supersedes that mechanism description.
