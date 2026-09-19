# Ownership: host, workspace, session, slot

Who owns a language-server process, who may use it, and what serializes
against what. Implemented in `src/host.rs`, `src/host/lifetime.rs`,
`src/workspace.rs`, `src/workspace/slot.rs` and `src/session.rs`.

## The four types

**`Host`** — one process's workspaces, keyed by canonical root, plus a
`released` registry of weak references to workspaces a session may still
hold after its agents let go. One host serves both front ends: the MCP
process owns a host with one default workspace; an embedded consumer owns one
host shared by its agents, each with its own effective cwd. Separately
spawned MCP processes share nothing.

**`Entry`** — one root and the agents interested in it. It owns the root, the
slot handles, the interested set, the workspace behind its own mutex, and a
clone of the host's shared `Lifetime`. The root and the slots are held
*outside* the workspace mutex on purpose: a launch reads them and moves a
server's lifecycle state without the lock that publication needs.

**`Workspace`** — the canonical root, one `ServerSlot` per **planned**
server, and the running server for each, indexed alike: `servers[i]` is
`Some` exactly when slot `i` is `Running`, and both change together under
the workspace lock. Building one is cheap and starts nothing — it reads the
tree's evidence and `structural.toml`, asks selection what to run, and lays
out a slot per plan (`discovery.md`) — so a workspace can answer routing
questions before anything is running. A `structural.toml` that is present
but unparsable is kept as a `config_error`: nothing starts, and every call
says why.

A slot per plan rather than per configured adapter is what keeps an
irrelevant server out of the process table: a Python-only tree lays out no
Rust slot, so no Rust server is warmed, demanded, or reported as down.
Evidence is read **once**, when the workspace is built; a tree that grows a
language afterwards keeps the slots it was laid out with, and a query for
the new language is told which server was left out and why. Noticing the
change while the workspace is live is observation's
(`../backlog/discovery/observation.md`).

**`Session`** — a caller's binding to one `Entry`, and the working
directory it was bound with (its **view**). A tool is handed
`&mut Session` and takes the workspace guard for the duration of its call.
The workspace is keyed by the project root above the view, so sibling
members of one project share servers; the view is what `path` resolves
against and what answers are filtered and rendered against, so each caller
is still answered about its own directory (`discovery.md`).
Two callers on the same root share one workspace, and so one set of server
processes; two on different roots lock independently.

The host map lock is taken only to find or create a workspace, never to run a
query. Queries within one workspace are serialized by that workspace's mutex
— the guarantee `&mut State` used to give the single-threaded MCP dispatcher
— and unrelated workspaces do not serialize against each other at all.

## Canonical roots

A workspace is keyed by `canonical_root`: two callers naming the same tree
through a symlink, a relative path, or a trailing slash land on one
workspace. A root that cannot be canonicalized (it does not exist) is kept
as given, for the error to report. `structural.toml` is read from the root,
so the canonical root also determines the effective configuration and is a
sufficient key today; it grows a configuration component the first time
configuration can come from somewhere else.

## Interest, not residency

`Host::agent_started(agent, cwd)` binds an `AgentId` to the workspace at
`cwd`, creating it on first use, and returns a `Session`. Interest is
recorded before the map lock is released, so a concurrent `agent_ended`
cannot release an entry out from under a caller that has not bound yet. The
eager warm that follows happens *outside* the map lock, so one workspace's
startup never queues another's work behind it.

`Host::agent_ended(agent)` removes that agent's interest wherever it was
recorded. Releasing one agent leaves another's session and the shared servers
untouched. When the last interest goes, the workspace leaves the map and a
**weak** reference to it is kept in `released`: the session still holding it
keeps it alive, the registry does not extend its life, and shutdown can still
reach the servers. Without that registry, a long-lived host could hold
children that outlived every agent until the holding session happened to drop
— which may be never.

Servers stop when the last session holding the workspace drops, or at
shutdown. There is no idle eviction and no expiry clock
(`../backlog/discovery/observation.md`).

## Slots: one server's lifecycle

A `ServerSlot` exists for every **plan** the workspace selected, whether or
not a server is running — the routing question ("which files would this
server serve") is answered by the adapter, so a *cold* slot still answers
it without a lock.

A slot holds the plan's whole **ladder**: the preferred candidate and the
alternatives behind it, in rank order (`discovery.md`). Which one the slot
stands for is an index, read without a lock because the list is immutable,
and the launcher moves it as it walks. So a slot's name, its install hint
and the languages it routes by are always the candidate actually in play.

```text
Cold ──demand──▶ Starting ──published──▶ Running
                     │
                     └──failed(reason)──▶ Failed
```

- **Cold** — configured, not started. A demand starts it.
- **Starting** — a launch is under way. A second demand waits rather than
  starting another.
- **Running** — the workspace holds the server.
- **Failed(reason)** — every candidate on the ladder failed, and `reason`
  names each one. A demand reports that rather than retrying; clearing it
  belongs to whatever can re-acquire an expired server, on the clock that
  expires one — neither exists yet.

The state sits behind the **slot's own** lock, not the workspace's, which is
what lets a server start while the workspace is otherwise free.
`ServerSlot::demand` decides under that lock, so becoming the launcher is
atomic with leaving `Cold`, and returns one of: `Ready`, `Join`, `Failed`,
`Lead(Launch)`, or `Refused` (the host is stopping — the slot is left exactly
as it was, so a demand after shutdown costs a load rather than a thread).

`mark_cold` is the one way to say "nothing is running here and a later demand
may start one again", used by shutdown and by any future eviction alike, so
the two cannot disagree about what stopping means. It resets the ladder with
the state, so a later demand walks it from the top rather than inheriting a
verdict on candidates that were tried in another launch. Every transition out of
`Starting` notifies the slot's condvar, so a waiter is never blocked on a
launcher that will not wake it.

## The launch path, and why the lock ordering is what it is

`Session::acquire(path)` is one function on purpose: it takes admission,
resolves the tool's relative `path` against the root, **activates** the
servers that scope needs, and only then takes the workspace lock for the rest
of the call.

`Entry::activate` starts every needed launch *before* waiting on any, so the
latency is the slowest server rather than the sum. Each launch runs on its
own thread and holds **no lock** while it spawns the child and performs the
`initialize` handshake; it takes the workspace lock exactly once, to publish.
That is why activation must happen before the workspace lock is taken: a
caller holding the lock and waiting for a launch would be waiting for itself.

One launch may start more than one child: `host::launch` walks the slot's
ladder, and a candidate that spawns but fails its handshake is stopped and
the next one tried. The walk is bounded by the list and every rejected
candidate is retained (`discovery.md`); only publication differs from the
single-candidate case, and it happens once, for whichever candidate
initialized.

Activation is best effort. A server that will not start is recorded on its
own slot, and the call goes on to ask whatever is available; what is missing
is reported by the query, which knows the scope. Bailing instead would let
one broken server stop every other from being warmed.

The `Launch` guard owns its slot and leaves `Starting` on **every** exit path
including a panic (`Drop` marks it failed), so no waiter can be stranded on a
launcher that died. A thread that could not even be created takes the closure
— and the guard inside it — with the error, so there is nothing to undo.

Publication is bounded. A launch that cannot take the workspace lock within
`publish_bound` (default 30 s, `Host::with_publish_bound`) stops the child it
started rather than queueing behind a query that may hold the lock
indefinitely. `std::sync::Mutex` has no timed acquisition, so `lock_within`
polls; polling is the honest shape, because the thing being bounded is
another thread's critical section, which nothing will notify us about. This
bound is a parameter on the host because what it trades is visible from
outside.

A poisoned workspace lock is ignored rather than fatal (`into_inner`): the
workspace's invariants are re-established by the next `sync_owned`, and
failing every later call would turn one panic into a dead session.

## Routing a scope to servers

`serving(slots, scope)` is the single routing rule, in one place because two
callers need it for different reasons — a launch asks which servers to start,
a query asks which to send to — and they must never disagree:

- a scope naming an existing **file** selects the slots whose adapter knows
  that file's language;
- anything else — no scope, a directory, a path that is not a file — selects
  every slot;
- a file with **no extension** that matches nothing selects every slot, since
  there is nothing to route by.

`Workspace::select` then narrows to the *running* ones and, when none is
running, produces the error: a file names the servers that should have served
it and their states; a bare scope names the whole configuration, the config
error if there is one, and each server's install hint.

## Sharing, in one sentence each

- Two callers on one root share one workspace and one set of processes.
- Concurrent demand on a cold slot produces exactly one launch, by the slot's
  election rather than by any coarse lock.
- Releasing one agent's interest leaves the other agent's session working.
- Unrelated roots neither share processes nor serialize.
- An operation already under way is never cut short by a release: the
  `Session` holds an `Arc` to the entry.
