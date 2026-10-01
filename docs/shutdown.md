# Shutdown

Best-effort graceful shutdown, escalated by the caller. Implemented in
`src/host.rs`, `src/host/lifetime.rs`, `src/lsp/handle.rs` (+
`handle/owner/lifecycle.rs`, `handle/child.rs`) and `src/main.rs`.

Priorities, in order: do not leave owned server processes running after a
controlled shutdown; avoid forcibly interrupting filesystem operations when
practical; keep the implementation proportionate.

## Three operations, not one

| call | does | blocks |
| --- | --- | --- |
| `request_shutdown()` | closes admission, stops children that are still starting | no |
| `force_shutdown()` | interrupts starting **and** running server work, bypassing protocol grace | no |
| `wait_shutdown()` | waits for admitted work, then sweeps every workspace's servers | yes |

`shutdown()` is the convenience soft-request-plus-wait. Requesting must never
block on a workspace lock, protocol I/O, or completion — otherwise escalation
could not be issued while a soft shutdown is waiting, which is the whole
point of splitting them. Force is idempotent and monotonic: repeated requests
share one shutdown state, and a force during a wait escalates that same
shutdown rather than starting a second.

Force means *force the servers*, not kill structural's own thread. The
process stays alive to unwind calls, reap children, and report results. A
force-interrupted call returns an explicit failure; it never returns a
successful partial answer.

## `Lifetime`: admission and reachability

`Lifetime` is cloned into every entry — shared state, not a copy, which is
what makes "one claim" true across workspaces, including a workspace acquired
*after* shutdown began. It holds a `Swansong` (completion tracking), a
`stopped` flag, and a registry of stop capabilities.

- **`admit()`** returns a `swansong::Guard` or refuses. It takes the same
  lock that closing admission takes, so completion cannot miss a participant
  that was about to start. Tool calls and launch elections both go through
  it, so an admitted call and a registered launch are covered by the same
  contract.
- **`stop()`** sets the claim **and** initiates the shutdown in one
  operation. Separating the two once cost an afternoon of hanging: a wait for
  a completion that was never initiated is indistinguishable from a slow
  shutdown.
- **`register(stopper)`** records a child's `Stopper` — the ability to say
  stop, and nothing else — as soon as it is spawned, so a shutdown can reach a
  child no workspace holds yet. It **refuses once stopping, and that check is
  inside the children lock on purpose**: reading the flag first and taking
  the lock second reopens the window it closes. A refused registration stops
  (or forces) the child on the spot.
- The `Registration` lives with the launch and then with the published
  server, through teardown, and marks itself `published()` when the server
  lands in a workspace. A stopped old instance dropping its registration
  cannot remove a replacement's capability, because the registration is keyed
  per launch.

## The disjunction that makes it safe

> **Every spawned child is either published and stopped by the sweep, or
> stopped by the launcher that spawned it.**

It holds because the claim is set before the sweep, and re-read by the
launcher inside the same workspace-lock hold that publishes: a launcher
publishing after the sweep would have had to see a clear flag that no longer
existed. The completion guards carry the *return* condition, not this
invariant.

Two consequences that look like details and are not. The shutdown wait is
what makes the sweep's `Cold` final — a launch returns its guard only after
it has written to its slot, so no slot write lands after a `mark_cold`; and
the sweep takes each workspace's servers out **under** its lock but reaps
them **outside** it, because dropping a server joins threads and that lock is
the one a launcher needs to publish.

## Reaching a stuck call

A tool holds the workspace lock while it makes blocking LSP calls, and a
server can answer liveness probes indefinitely without ever answering the
request ([correctness.md](correctness.md)). So soft shutdown can wait
forever on that call — by design, since the alternative is a timer that
returns a wrong answer. Force is the escape: it reaches starting and running
servers through their `Stopper`s, which are held by the `Lifetime`, **not**
through the workspace, so it takes no workspace or host-map lock that the
waiter may hold.

## Process ownership

`Stopper::stop` requests graceful LSP teardown; `Stopper::force` skips the
remaining grace window and abandons pending work. Teardown converges on one
path: a bounded graceful `shutdown`/`exit` attempt, then kill and reap the
child **before** joining the writer (a write blocked on a wedged server
unblocks only when the pipe closes), then bounded joins. Every waiter still
registered is failed with the reason; none is left hanging.

On Unix each server starts in its own **process group**; cleanup signals the
group before reaping its leader, and keeps the group identifier pinned until
signalling is finished so a stale id can never be signalled. Grandchildren
are terminated, not reaped by structural — it only reaps its own children.
Descendants that deliberately detach are outside the guarantee. Other
platforms retain direct-child cleanup only. Stop capabilities carry flags,
never PIDs, so they stay safe to hold and call after teardown.

## The front end: signals and EOF

Signal handlers are registered **before** root resolution and eager
startup, so a signal during startup is not missed. On Unix the first
SIGINT/SIGTERM/SIGHUP requests a soft shutdown and any subsequent signal
forces; no signal path performs blocking teardown and none calls
`process::exit`.

`serve` reads stdin through a poller that waits on stdin and a cancellation
descriptor together, which separates two things that look alike:

- **Real input EOF** (the harness went away) forces cleanup **in the
  reader**, not after dispatch returns — dispatch may still be blocked on a
  server that will never answer.
- **Synthetic cancellation** after a soft cleanup completes is not
  escalation: a cleanup thread ends input once admitted work has finished, and
  the serve loop drains its responses and returns naturally.

One-shot CLI dispatch (`structural definition choose`) keeps its behaviour: the
same `host.shutdown()` runs on the way out, and a failed run forces first.

## Limits, stated rather than fixed

- No crash-proof multi-file edit transaction. Soft shutdown protects a whole
  admitted tool call, including a rename's writes; a **forced** interruption
  can leave a multi-file action partly applied, and the failure says so
  rather than claiming a rollback.
- A caller that retains a raw `Session::workspace()` guard forever cannot be
  made to release it. `Session::acquire` is the supported path and releases
  the workspace before returning admission; the raw guard is a low-level
  escape hatch whose caller must release it before waiting.
- Signal support is Unix-specific. Uncatchable death of structural itself
  (SIGKILL) needs an external supervisor or an OS-specific parent-death
  mechanism; no in-process destructor can make that reliable.
- An embedding application that reaps structural's children independently is
  outside the contract.

Regressions are tested through a **real subprocess**, not inferred from host
tests: the first frontend implementation passed every host test and still
hung forever in `read` at EOF ([../journal/2026-09-16-shutdown-escalation.md](../journal/2026-09-16-shutdown-escalation.md)).
