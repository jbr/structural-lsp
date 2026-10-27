# The operator channel

Parent: [README.md](README.md). Implemented in `src/operator.rs` (the
event, the sink trait, the stderr default), `src/operator/mcp.rs` (the MCP
front end's sink), `src/host/report.rs` (what an entry reports, and when),
and at the transitions in `src/host/launch.rs` and
`src/host/observation/mod.rs`.

Some facts belong to whoever runs structural and not to the agent calling
it. A configured server that would not spawn, a ladder that fell back, a
language in the tree that nothing serves, a server the idle sweep released,
a filesystem watch that was lost — an agent asked to rename a function can
act on none of them, and every byte of tool output is paid for on every call
([correctness.md](correctness.md), invariant 5). They go to the operator
instead, pushed at the moment they happen.

## What stays in the answer

A tool answer keeps every fact its caller can act on, as it did before
this channel existed: a scope with no symbol index says so, because naming
a file in `path` routes around it ([resolution.md](resolution.md)); a query
whose server failed names the server, its state, each candidate tried and
the install hint ([ownership.md](ownership.md)); a successful fallback is
in the result footer (`Workspace::descriptions`). Nothing in this channel
replaces one of those. A launch failure is therefore both: the query that
needed the server is told, and the operator is told once.

## Push, not pull

The channel has no tool. A status tool costs its schema and description in
every agent's context on every session, for answers the agent cannot use;
an intermittent status line on tool results costs that attention on the
calls that carry it, and was rejected for the same reason during the
observation unit. An event is pushed at the transition that produced it,
to a sink the host was constructed with.

## The event

`operator::Event` is structured, so an embedder can render it rather than
parse it:

- `level` — `Info` (a state change, nothing wrong), `Warning` (degraded and
  worked around), `Error` (nothing will work until someone fixes it);
- `kind` — which transition (`operator::Kind`, below);
- `root` — the workspace's canonical root;
- `server` — the language server it is about, when it is about one;
- `message` — one line saying what happened, with the reason and install
  hint where there is one.

Its `Display` is the stderr line:

```text
structural: error: /work/project: rust-analyzer: failed to start: starting rust-analyzer: spawning `rust-analyzer`: No such file or directory (os error 2); to fix: `rustup component add rust-analyzer`, and ensure it is on PATH
```

A line break inside a reason becomes a space, so every event is exactly one
line.

## Events, and where each is emitted

Every event is emitted at a transition, never per query. A query that
finds a slot already failed reports that to its caller and emits nothing.

| Kind | Level | Emitted | Server |
| --- | --- | --- | --- |
| `CandidateFailed` | warning | `Entry::launch`, when a candidate fails and another follows it on the ladder | the candidate |
| `LaunchFailed` | error | `Entry::launch`, when the ladder runs out — one event naming every candidate, reason and hint — and `Entry::publish`, when a started server cannot be published within the bound | the preferred candidate / the server |
| `Fallback` | warning | `Entry::publish`, when the server published is not the ladder's first | the server serving |
| `Unserved` | warning | `Entry::new` (the first selection) and the observer after each refresh; once per workspace per language | — |
| `Config` | error | `Entry::new`, when the configuration does not load — `structural.toml` present and unparsable, or the embedder's source erred | — |
| `Released` | info | the observer, after each idle sweep, per server it stopped | the server |
| `WatchLost` | warning | the observer, when the first watch fails or a working watch is lost | — |
| `WatchRecovered` | info | the observer, when a lost watch is re-established | — |

The last candidate's failure has no `CandidateFailed` of its own: it is
part of the `LaunchFailed` that follows, so a one-rung ladder that fails
reports once. A ladder walked again — after the idle sweep clears a failed
slot and a later query demands it — reports again, at most once per idle
interval. `Fallback` is reported before the slot reads `Running`, so a
caller woken by the publication finds it already emitted. A launch a stop
superseded marks nothing on its slot and reports nothing either
([observation.md](observation.md)).

`Unserved` comes from selection: a language the tree gives evidence for —
in practice a marker such as `pom.xml` or `build.gradle` — for which no
catalogue entry or configured server exists, or every one is disabled. It
is recorded on the `Selection` (`Selection::unserved`, with the evidence),
not declined, since there is no server to decline ([discovery.md](discovery.md)).
The entry keeps the set of languages it has reported, so a refresh that
still finds Java says nothing and a refresh that newly finds Kotlin reports
Kotlin.

Not reported: a published server whose process later exits or whose
connection fails. The transport records that failure on the connection
and every later query on that server reports it to its caller; no host
transition observes it, and the gap is
[`../backlog/operator-server-exit.md`](../backlog/operator-server-exit.md).
The front end's own shutdown notices (`structural: graceful shutdown
requested`) are written by `src/main.rs` directly and are not events.

## The sink

```rust
pub trait Operator: Send + Sync + 'static {
    fn event(&self, event: &Event);
}

impl Host {
    pub fn with_operator(self, operator: Arc<dyn Operator>) -> Self;
}
```

One sink per host, set before any agent binds; each workspace takes the
host's operator when it is created and holds it for its life. Without
`with_operator` the sink is `operator::Stderr`, one line per event. A
`Session::new` workspace — the command-line path and in-process tests —
reports to `Stderr`.

`event` is called on whichever thread made the transition: a launcher
thread, the observation thread, or the binding caller. `Fallback` is
emitted under the workspace lock (that is what orders it before
`Running`), so an implementation returns promptly and never calls back into
the host.

## The MCP front end

`structural-lsp serve` builds its host with `operator::mcp::Mcp`. Every event
goes to stderr at once, as the default sink writes it, and to the client as
MCP logging: a `notifications/message` with `level` (`info`, `warning`,
`error`), `logger: "structural"`, and `data` holding the event's fields:

```json
{"jsonrpc":"2.0","method":"notifications/message","params":{"level":"error","logger":"structural","data":{"kind":"launch-failed","message":"failed to start: starting rust-analyzer: spawning `rust-analyzer`: No such file or directory (os error 2); to fix: `rustup component add rust-analyzer`, and ensure it is on PATH","root":"/work/project","server":"rust-analyzer"}}}
```

The server declares the `logging` capability and answers
`logging/setLevel` (mcplease's `ServerConfig::with_logging`); mcplease's
connection notifier drops messages below the level the client set, and
sends every level until it sets one.

The binding's eager warm runs before the MCP connection exists, so the
earliest events — a server that will not spawn — have nowhere to go yet.
`Mcp` holds them until the front end attaches the connection's notifier,
which it does at the first tool call admitted once the client has
completed the `initialize` handshake (mcplease's `ConnectionNotifier`
delivers nothing before that, and nothing at all on a `2026-07-28`
connection, which has no notifications tied to no request). Attaching sends
the held events in order, ahead of the admitted call's answer and of every
later event. Events emitted after the handshake but before that first call
wait for it.

The hold is bounded at 64 events (`operator::mcp::HELD`). Past it an event
still reaches stderr and is counted, and attaching sends one warning
(`data.kind: "unheld"`) saying how many reached stderr only. On a
connection that never completes a handshake, everything stays on stderr.

## Falsifiable

- A launch failure reaches a recording sink exactly once, however many
  queries find the slot failed afterwards (`src/host/tests/operator.rs`).
- A fallback reports the failed candidate, then the server serving; an
  exhausted ladder ends in one error naming every candidate; a launch that
  goes as planned reports nothing (same file).
- A language with evidence and no server is reported once per workspace
  across refreshes, and a newly unserved one at the refresh that finds it
  (same file; `src/discovery/selection/tests.rs` for the selection side).
- The stdio front end writes a launch failure to stderr and sends it as a
  `notifications/message` ahead of the first tool call's answer
  (`tests/mcp_smoke/operator.rs`).
