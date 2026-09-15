# Server lifecycle: the plan for unit 2

Parent: `docs/lifecycle-and-discovery.md`, unit 2 — "Make server lifecycle
explicit". Predecessor: `docs/host-extraction.md` (unit 1, landed `5b3e903`).

This is the unit-level decomposition: the shape, the invariants that make it
safe, and the order to land it in. The parent doc stays the authority for what
the unit is for.

## Done when

Quoted from the parent plan:

- concurrent demand causes one launch;
- expiry cannot interrupt an operation;
- later demand starts a fresh instance;
- one failing server does not block another;
- shutdown during startup leaves no orphan.

Plus: explicit starting/running/failed/stopping state, deduplicated
acquisition, independent launches, in-flight protection, controlled
reacquisition, explanatory status.

**Not included:** discovery and the catalogue (unit 3), filesystem observation
(unit 4), LSP synchronization (unit 5), the per-call `workspace` argument
(unit 6), and concurrent semantic operations within one workspace — which the
parent doc keeps out of scope, so nothing here should depend on them.

## Where unit 1 left it

Facts from the code, not from memory:

- `agent_started` holds the **host map lock across `Workspace::new`**, which
  starts every adapter's server through `LspServer::start` — spawn and
  `initialize` for each, serially, each bounded by that adapter's backstop
  (90 s by default). That is the floor this unit replaces.
- A call holds `MutexGuard<Workspace>` for its whole duration. That is what
  makes release safe in unit 1, and it is what will make expiry safe here.
- **`Host::shutdown` drains two collections**, and both must keep being
  drained: the map, and the `released` registry of weak references to
  workspaces a session still holds after their agents released them. The
  registry exists because a detached-but-held workspace is otherwise invisible
  to shutdown and its children are orphaned; this unit's shutdown must not
  quietly drop it.
- `LspServer` owns both the handle and `opened: HashMap<uri, OpenedDoc>`.
  `sync_owned` iterates **only** the keys in `opened`, and `opened` starts
  empty on any fresh instance.
- `Adapter: Send + Sync` is a cheap value object with everything defaulted,
  and `LspServer::start` consumes `Box<dyn Adapter>` by value.
- `LspServer::start` spawns the child *and* performs the `initialize`
  handshake before returning, so no caller can reach the child while it is
  initializing.
- Readiness is `wait_ready`, bounded by the inactivity backstop. The backstop
  is the only **per-request** timer; `shutdown_grace` and the derived
  `join_bound` govern teardown only, which is what keeps it distinct from
  expiry.

## Shape

```text
Host ─ map lock: root → Arc<Entry>          lookup, insert, release only
  │
  └─ Entry ─ workspace: Mutex<Workspace>     semantic and document state
           └ slots: Vec<Arc<ServerSlot>>     one per adapter, fixed at creation
                └ state: Mutex<SlotState>    lifecycle only
```

- `SlotState` is `Cold | Starting | Running | Failed(reason) | Stopping`.
  `Running` means the workspace holds a server for this slot; the other states
  are recorded here so a caller can read them **without** the workspace lock.
- A slot keeps its adapter, because `select` must answer for a *cold* server:
  which languages it would serve is a property of the adapter, not the server.
  Both the slot and `LspServer` hold it as **`Arc<dyn Adapter>`**, so a launch
  is a reference bump and there is one source of truth. `Adapter` is not
  `Clone`, and a `clone_box` would need `ReadinessPolicy` to grow one too
  (`readiness_policy()` returns a `Box`), so the `Arc` route is both smaller
  and more honest: the adapter is shared configuration by nature. The cost is
  a mechanical signature change through `Config::adapters()` and the start
  call sites.
- Servers stay inside `Workspace`, so semantic work takes one lock, exactly as
  today.

## Locking rule

Three locks, taken in this order, outer to inner. **Never** take an outer lock
while holding an inner one.

1. the host map lock — lookup, insert, release, and nothing else;
2. the workspace lock — semantic and document state, held for a call;
3. a slot's state lock — lifecycle only, never held across protocol I/O.

Keep the pose that has served both units so far: an operation's protection is
an accident of a lock it already holds, not a separate guard.

**Publication is one workspace-lock hold.** The abort-flag check, the push of
the server into the workspace, and the slot's move to `Running` must all happen
inside it. If the slot update were deferred until after the lock dropped, a
waiter could observe `Starting` for a server the workspace already holds, and
`select` and activation would disagree with reality until the update landed.

## Activation happens *before* the workspace lock

The one structural rule that makes dedup safe:

```text
call → activate(scope)     may spawn and wait; holds no workspace lock
     → workspace lock      the servers it needs are now Running
     → select, query, release
```

Activation may block on a launch, so it must not hold the workspace lock;
publication takes that lock only briefly. If activation waited while holding
the workspace lock it would deadlock against its own launcher — the failure the
naive "reacquire = take the workspace lock, start the server" always produces,
and the trap to avoid.

Dedup falls out of election on the slot state lock: the first arrival takes
`Cold → Starting` and becomes the launcher; every other arrival for that slot
waits on the slot's condvar. One launcher per slot; the launcher holds no lock
during spawn or `initialize`, and publication is the short critical section
above.

**Every launcher exit path must wake the slot.** Election sets `Starting`
under the slot lock, but the transition out of it happens later, in the
launcher, holding the workspace lock — so a panic anywhere in between would
leave the slot `Starting` forever with no launcher, and every waiter blocking
on a wake that never comes. An RAII guard owns that transition: on success it
sets `Running`, on a launch error it sets `Failed(reason)`, on unwind it sets
`Failed("launcher died")`, and in every case it notifies. **Waiters wake on
every terminal state, never only on `Running`.**

Waiters are std `Condvar`, not tokio. The codebase is std threads end to end.

## Expiry

The workspace lock *is* the in-flight protection, and that forces the
granularity:

- eviction needs the workspace lock, so a call in progress cannot be
  interrupted by expiry — true by construction, the same accident as unit 1's
  release safety;
- therefore eviction is **per workspace**, not per server. A workspace with an
  active call keeps its idle servers until that call finishes. Do not design
  for finer granularity; the lock structure forbids it.
- the sweep uses `try_lock`: if the workspace is busy, skip it and let the next
  tick try again. The janitor never blocks on a call.
- eviction **removes** the server under the lock and shuts it down *outside*
  it, so a graceful stop (up to `shutdown_grace`) does not hold the workspace.

The clock is deliberately minimal: `Clock { fn now(&self) -> Instant }` plus
`Host::expire_idle(now)`, which tests call directly. Production runs a janitor
thread that ticks and calls it; tests advance a value and call it, and never
sleep. No scheduler, no timer wheel, no wake-on-deadline: this unit has nothing
that must *happen* at a deadline, only within a tick of one, and a 30–60 s tick
against a 30-minute idle target is noise.

The janitor has a lifecycle of its own and the plan says so: it holds a
`Weak<Host>` (or consumes a stop signal that `Host::shutdown` sets) and is
joined on drop, so it cannot outlive the host it expires servers for or keep it
alive. A janitor racing `shutdown` is sequentially benign, because both
serialize on the workspace locks — but that is a sentence in the code, not an
accident.

`last_activity` is per slot, bumped by any request, notification or sync
through that server — strong signals only, as the parent doc requires for this
unit. Filesystem activity is not consulted.

The idle timer and the inactivity backstop stay in different modules with
different names: the backstop is per-request, real-time, inside `ServerHandle`,
and measures server health; expiry is host-owned policy on an injected clock
and measures use. The project's "no timers except the backstop" convention
becomes "the backstop and the idle sweep", naming both — which the parent doc
already anticipates.

## Failure policy

`Failed(reason)` is terminal for the call that finds it and retried by the next
demand. A call that finds `Failed` returns the actionable error immediately
(the same fast-fail the MCP surface gives today for an unavailable server); the
next call elects a launcher and waits for a fresh attempt.

This is not the 90-second hang it looks like. A missing or unrunnable binary
fails `LspServer::start` in milliseconds — only a *hung* `initialize` costs the
backstop, and that is the case where waiting is correct. No backoff, no retry
timer, no restart loop: demand is the signal.

Failures are per slot, which is what makes "one failing server does not block
another" true — a waiter on one slot is untouched by another's failure.
`config_error` stays workspace-level: an unparsable `structural.toml` still
means nothing starts and every call says why.

This whole policy is milestone A's, not B's: a `Failed` state with no way past
it is a tombstone a call can never clear, and retry-after-failure is also the
first path that produces a fresh instance carrying an empty `opened` map.

## Shutdown during startup

The invariant to state and to test:

> every spawned child is either published — **into the map or the released
> registry** — and swept, or killed by the launcher that spawned it.

Publication has two destinations, because unit 1 established that a workspace
released by its agents can still be held by a live session. A shutdown that
swept only the map would orphan exactly the children the `released` registry
was added to catch.

The ordering:

1. `shutdown` sets the abort flag on **every** slot first, taking only slot
   state locks;
2. it terminates the child of every launch still in flight;
3. it waits for in-flight launches to drain, on a launch counter and condvar;
4. it takes each workspace lock and stops everything published, in the map and
   in the released registry alike.

**Why the flag-before-sweep is what carries the invariant.** The launcher's
check-then-publish and the sweep both need the workspace lock, so they cannot
interleave; a launcher that publishes after the sweep would have had to see a
clear flag that no longer existed. A launcher that finds the flag set kills the
server it just built and publishes nothing. The counter in step 3 does *not*
carry the invariant — it carries the **return** condition.

**Why the return condition is worth paying for.** An MCP process's normal path
is `host.shutdown()`, then exit. A launcher still holding a live child at that
moment has no later chance to kill it — the thread dies with the process, and
the child's own exit on stdin EOF is not something a Done-when clause can rest
on. So shutdown must be decisive before it returns.

**Why step 2 exists, and what it buys.** Without it, step 3's wait is bounded
by the backstop — up to 90 s of exit latency for one hung `initialize`. Ending
the in-flight child makes its `initialize` fail at once, so the drain is bounded
by teardown (seconds) instead. Step 2 needs two small changes this unit already
has to make:

- `ServerHandle::terminate(&self)` — the flag and the graceful command without
  the join, which is what `shutdown(self)` already does before joining;
- `LspServer::start` split into `spawn` and `initialize`, so the handle exists
  and is registered with the slot *before* the slow handshake. `LspServer`
  holds `Arc<ServerHandle>` to make that registration possible.

Shutdown must never block on a slot's waiters; launchers clean up their own.

## Document state across reacquisition

`opened` is per `LspServer` and starts empty, so a reacquired instance knows
nothing about documents the previous one had opened. `sync_owned` on a fresh
server is a no-op. The contract is per-call resync: `sync` opens on first touch
for the file being queried, so the common path is self-healing.

This is a quiet staleness bug if left implicit, and it gets a test: a bare-name
query immediately after a reacquisition must still resolve. It belongs to
milestone A, because retry-after-failure reaches it without any clock.

## Milestones

Two halves, each green and each useful on its own. The parent doc permits a
work unit to span commits.

**A — explicit lifecycle, lock-free launch, dedup, failure, shutdown.**
Slots, `SlotState`, the `Arc<dyn Adapter>` change, activation before the
workspace lock, election and `Condvar`, the launcher's RAII guard, the abort
flag, `ServerHandle::terminate`, the `spawn`/`initialize` split, and
`select` learning to answer per slot. Per-slot failure with demand-driven
retry, and the document-state contract with its test. No clock, no expiry.

Covers: one launch, independent launches, one failing server does not block
another, no orphan.

**B — expiry, reacquisition, status.**
`Clock`, `expire_idle`, `try_lock` eviction, the janitor and its lifecycle,
per-slot `last_activity`, and the status rendering below.

Covers: expiry cannot interrupt an operation, later demand starts a fresh
instance.

**Status is rendering, not a surface.** `unavailable` disappears; the existing
`nothing_running` path learns to name three cases — failed (reason plus the
install hint), starting, and nothing configured. No status API, no new MCP
tool, no footer redesign; whether operational status deserves a tool is a
parent-doc decision and is not this unit's.

## Cut

- per-server eviction granularity (the workspace lock forbids it anyway);
- reacquisition as a *policy* layer — it is demand-driven or it does not happen;
- `Stopping` as a waiter-visible state (a label during a short critical section
  is enough; no caller waits on it);
- any lazy-start policy. Agent start keeps warming every configured server
  eagerly, as today, because rust-analyzer's indexing should begin before the
  first query. Unit 2's acquisition work is dedup-at-warm plus
  post-expiry/post-failure reacquisition, not a move to lazy start.

## Traps, ranked

1. **The launcher holding the workspace lock during startup.** Invisible until
   a query-during-reacquisition deadlocks. Activation before the lock is the
   whole defence.
2. **Document state assumed to persist across instances.** Silent staleness
   after a reacquisition; see above.
3. **A launcher that dies without waking its waiters.** The slot sticks at
   `Starting` forever; the guard above is the defence.
4. **Sticky-failure policy.** Retry-on-a-timer hangs calls for the backstop;
   never-retry kills a server permanently on a transient failure. Demand-driven
   retry is the only one that is both fast and recoverable.
5. **Scheduler-framework creep.** One sweep, one predicate, an injected `now`.
6. **Async infiltration.** `Condvar`, not `Notify`.

## Deliberately deferred

- **Discovery and the catalogue.** Slots are built from today's configured
  adapters alone; unit 3 replaces the source of that list.
- **Observation.** A workspace's evidence lifetime is separate from server
  residency, and that separation is unit 4's.
- **Per-call workspace selection.** One default context per caller, unit 6.
- **Eviction of the workspace itself.** This unit stops *servers*; an entry
  whose interest is empty is still released by unit 1's rule.
- **Concurrency within a workspace.** One call at a time, as now.

## Verification

- Lifecycle, dedup and shutdown are tested with the shared Python mock from
  `src/test_support.rs`, which records each launch, so "one launch" and "no
  orphan" are assertions about recorded pids rather than about timing.
- Expiry is tested by calling `expire_idle` with an advanced `now`. "Expiry
  cannot interrupt an operation" is deterministic under this design: hold a
  workspace guard in the test and call `expire_idle` — the `try_lock` skips it.
  No test sleeps through an interval.
- A launcher panic is tested by failing one deliberately and asserting the
  slot reaches `Failed` and the waiter returns rather than hanging.
- One real-server test walks the reacquisition path end to end, since the
  document-state contract above is exactly what a fake server would flatter.
- `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo doc
  --no-deps`, and the ignored real-server suite stay green at each milestone.