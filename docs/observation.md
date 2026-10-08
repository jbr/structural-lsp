# Observation: the event source a workspace watches itself with

Parent: [README.md](README.md). The parts of the unit not yet built — the
strong/weak signal policy, activation for interested workspaces, and server
residency separated from agent interest — are
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
  not act as a keepalive (`../backlog/discovery/README.md`'s interest
  rules).
- A backend error the reduction cannot interpret, and a platform "events
  were dropped" notice (`Event::need_rescan`), arrive as `Overflow` —
  which reduces to the same rescan, so "lost" and "received" cannot be
  told apart and never need to be.
- A refresh appends only. A language whose evidence dropped keeps its
  slot, which serving then stops matching; eviction is later work. And a
  refresh warms nothing: a new slot starts when a query demands it.
  Whether an interested workspace's new language warms without a query is
  the signal policy, not this reduction.

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
