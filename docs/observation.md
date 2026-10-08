# Observation: the event source a workspace watches itself with

Parent: [README.md](README.md). The part of the unit not yet built — server
residency separated from agent interest: idle eviction, reacquisition, and
eviction that cannot break an in-flight launch — is
[`backlog/discovery/observation.md`](../backlog/discovery/observation.md).

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
for. Nothing from the batch survives the reduction; the paths are carried
for the day something selective is worth proving, not used today. A lost
event is therefore reconciled by construction rather than trusted: the
rescan rebuilds what the event would have said.

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
  slot, which serving then stops matching; eviction is later work. And a
  refresh itself warms nothing — it returns the languages that are new
  with their slot indices, and whether they warm is the signal policy
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
The reactivation-after-eviction row of that table is residency work: a
workspace has nothing stopped to reactivate yet.

The wiring that makes this reachable: the observer holds the workspace
lock only for the refresh itself and grows the entry's slot copy
(`Entry::sync_slots`) in the same critical section — so the indices
`refresh` returns name the slots the demand path reads without the lock,
and a language the tree gains is answerable by a query immediately, not
just by the next bind. The interest check happens after the refresh: an
agent binding between refresh and warm is covered anyway, because its
bind warms the whole (grown) list.

## The thread holds the workspace loosely

The observation thread holds only a `Weak` to the workspace's entry: no
`Arc` survives a park, and the upgrade is re-checked before parking and
after every batch. A released workspace is not kept alive by its own
watcher; when its tree next makes a noise, the thread notices the release
and exits. A workspace whose tree then goes quiet leaves the thread parked
with a live watcher until that next event — eager teardown of released
watchers is residency work, not this reduction's.

Watch errors end observation quietly: one stderr line, and the workspace
behaves as it did before there was a watcher — evidence goes stale. A
watcher existing is not a claim about anything: no batch, and no watcher
at all, establishes that an answer is current
([`synchronization.md`](synchronization.md) is that contract, and it is
not built).
