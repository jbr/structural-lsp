# Observation: the event source a workspace watches itself with

Parent: [README.md](README.md). What a query may claim about currency is
synchronization's contract, not this one: [`correctness.md`](correctness.md)
states the query boundary, which reconciles each server against the tree
without waiting on this watcher.

Every workspace gets an event source when it is created
(`Host::agent_started`): a thread that watches the workspace root and, per
batch of filesystem events, re-runs discovery. The default source is the
`notify` crate's platform watcher; tests install a synthetic source the
test drives (`src/host/tests/observation.rs`), so nothing here is asserted
against watcher timing.

## Every batch reduces to one thing

A batch — whatever it carried, including an overflow — means exactly
"re-read the tree": `Workspace::refresh` re-gathers evidence under
[`FILE_BUDGET`](discovery.md), re-runs selection under the layout the
first selection used, and appends slots for languages the tree newly calls
for. A language the new selection finds present and unserved is reported
to the operator, once per workspace ([operator.md](operator.md)). Nothing from the batch survives the reduction; the paths are carried
for the day something selective is worth proving, not used today. A lost
event is therefore reconciled by construction rather than trusted: the
rescan rebuilds what the event would have said.

The watcher's callback fills two buffers, and the two consumers of its
events cannot take an event from each other. This thread's coalesce
drains the observation buffer into batches, as above. The query path
drains the other at the query boundary, so a query is told every change
the watcher delivered before the query was admitted — the boundary
[`correctness.md`](correctness.md) states, and the flush that implements
it. What this thread reduces is evidence; what the query boundary sends
is the server-facing notification; neither sees the other's copy.

Events coalesce over a half-second window, bounded in size. The window
bounds the reduction's latency and triggers nothing — it is not a timer in
[`correctness.md`](correctness.md)'s sense, and nothing waits on it for an
answer.

- Access, `Any`, and `Other` events are dropped at the source: a file
  being looked at is not the tree changing, and filesystem activity must
  not act as a keepalive (the signal policy, below).
- A backend error the reduction cannot interpret, and a platform "events
  were dropped" notice (`Event::need_rescan`), arrive as `Overflow` —
  which reduces to the same rescan, so "lost" and "received" cannot be
  told apart and never need to be.
- A refresh appends only. A language whose evidence dropped keeps its
  slot, which serving keeps matching — removal is the idle sweep's, on
  the clock that expires a server, not evidence-driven. And a refresh
  itself warms nothing — it returns the languages that are new with
  their slot indices, and whether they warm is the signal policy
  (below), decided after the refresh returns.

## Signals: strong start, weak refresh

A signal is strong or weak by what it may start.

**Strong** — an agent binding to the workspace, and any tool query.
Binding warms every planned slot; a query starts exactly the servers its
scope routes to and waits for readiness. A query is strong whether or not
an agent is attached: a later call reacquires what it needs. Both run
through the entry's copy of the slot list, so they never wait on the
workspace lock to route.

**Weak** — a filesystem batch. It always refreshes evidence and grows the
slot list; it starts a server only for a language the tree newly calls
for, and only while the workspace has an attached agent. An observed-only
workspace — released, still held by a session — updates evidence and
launches nothing. And plain activity never relaunches anything: a batch
that changes nothing, or an edit in a language the workspace already
serves, does not resurrect a stopped or failed server. Filesystem
activity is not an unconditional keepalive — build output and background
generators must not keep a process alive or bring one back
(`../backlog/discovery/README.md`, the signal table it was agreed from).
Reactivation after eviction is the demand path itself: the next query
acquires what it needs, exactly as it would on a cold slot (residency,
below). An edit never does — the row's answer is that a strong signal
reactivates and a weak one never.

The wiring that makes this reachable: the observer holds the workspace
lock only for the refresh itself and grows the entry's slot copy
(`Entry::sync_slots`) in the same critical section — so the indices
`refresh` returns name the slots the demand path reads without the lock,
and a language the tree gains is answerable by a query immediately, not
just by the next bind. The interest check happens after the refresh: an
agent binding between refresh and warm is covered anyway, because its
bind warms the whole (grown) list.

## Residency: the idle sweep

Server residency is separated from agent interest: a server runs while
strong signals keep arriving addressed to it, and stops when they stop,
while observation goes on.

- **The clock is per slot.** Each slot carries the moment it was laid
  out, refreshed by every strong signal addressed to it — a bind
  addresses all of a workspace's slots, a query the ones its scope
  routes to, and a filesystem batch none. A Python-only stretch lets
  `rust-analyzer` fall out of a polyglot workspace while `pyright`
  stays. Filesystem activity is not a keepalive, and neither is mere
  attachment: an agent that has addressed nothing for the interval
  loses its servers, and its next query pays what any fresh bind
  would.
- **The interval is one, not a table.** Default 30 minutes
  (`crate::config::DEFAULT_IDLE_EVICTION`), overridable per workspace
  by `idle-eviction-secs` in `structural.toml` — the file every other
  workspace-level key lives in, not per-server.
- **The sweep runs on the observer thread.** The thread's park is
  bounded by the next slot's expiry, computed without the workspace
  lock from the entry's own slot copy; every wake — batch or park
  spent — ends with a sweep of what has since expired. A spent park is
  an empty batch: the reduction runs on real events only. Eviction
  stops servers, never observation, and each server it stops is
  reported to the operator as `Released` ([operator.md](operator.md)).
- **The sweep stops `Running`, `Starting`, and `Failed` alike.** A
  `Failed` slot's verdict expires on the same clock, so a failure is
  never retried in a loop and is not permanent either: the next demand
  walks the ladder from the top. A `Starting` slot whose launch
  outlives the interval is the one case that makes an in-flight launch
  stale — and the one case is answered below. Nothing removes slots:
  eviction re-arms, it does not shrink.
- **A launch superseded by a stop cannot publish over the slot.** Each
  slot carries a generation, bumped by every stop (`mark_cold`); a
  launch elected from `Cold` records the generation it was elected
  under, and publication — under the workspace lock, where bumps also
  happen — compares before it writes. A superseded launch drops the
  child it started and exits without marking the slot: the state there
  belongs to whatever stopped it, or to a launch elected after. Never
  a bare `Starting` check — that is not identity across a
  stop/restart.
- **Reacquisition is the demand path.** `mark_cold` leaves the slot
  cold; the next query's warm elects a launcher and waits for
  readiness, which is also how a fresh bind behaves. A query that
  races an eviction may see the slot stop under it and reports the
  state; the next call reacquires.

When the workspace is released — no agent interested — a session may
still hold and query it: its servers still expire on the same clock,
its evidence still refreshes, and nothing launches on a weak signal.
When the last session drops, the entry dies, and with it the watcher,
the thread, and any servers the workspace still holds.

## The thread holds the workspace loosely

The observation thread holds only a `Weak` to the workspace's entry: no
`Arc` survives a park, and the upgrade is re-checked before parking and
after every batch. A released workspace is not kept alive by its own
watcher, and it keeps observing — evidence maintenance and the sweep
both — until its last session drops, because a session holding it may
still query and demand still starts what a query needs. When the entry
itself dies, its `Drop` closes the source, and the thread ends at once
rather than at the tree's next event.

Overlapping subscriptions route correctly by construction: every
workspace watches its own root and reduces into its own evidence, so two
entries over one tree — the release window — never share mutable state.
Sharing one native watch across entries is an optimization, not owed
here.

A watch failure is not death. The source reports the loss, the thread
survives it, and the sweep keeps running — eviction needs only a clock —
while every sweep re-attempts the watch: a transient failure self-heals,
and a persistent one costs one attempt per interval. The loss and the
recovery are each reported to the operator once — `WatchLost` and
`WatchRecovered` ([operator.md](operator.md)); the retries between stay
quiet. Evidence
stops improving, which is the behaviour the workspace had before there
was a watcher. A watcher — present, lost, or absent — is not a claim
about anything: no batch establishes that an answer is current. What a
query may claim is [`correctness.md`](correctness.md)'s boundary, and
the cases it does not cover are stated there.
