# structural — architecture

> An MCP server that gives coding agents LSP-backed code intelligence:
> `definition`, `references` and `rename`, answered by real language servers rather than
> by grep or a bundled grammar. A model's view of code is a token stream, not
> an editor viewport — it pays per byte, cannot point with a cursor, and its
> memory of a file is a snapshot from N turns ago.

This is the top of the architecture tree: what structural is, how a call
flows through it, the invariants that keep answers correct, and a map to the
per-subsystem docs — each of which names the modules that implement it, so a
reader goes to the code rather than grepping for it.

These docs are present-tense: they describe how the system works now.
Planned work is in [`../backlog/`](../backlog/); the terse record of what
shipped is [`../CHANGELOG.md`](../CHANGELOG.md); the narrative of why a road
was taken is [`../journal/`](../journal/); how we work on the code is
[`../process/`](../process/) and `CLAUDE.md`; external research and datasets
are [`../reference/`](../reference/).

## What this is

One Rust binary. `structural serve` speaks MCP over stdio and exposes three
tools; `structural definition choose` runs the same tool once from argv. Behind
either front end sits a **host**: a process's workspaces, keyed by canonical
root, each owning the language-server processes for that root and shared by
every caller bound to it. The MCP front end is one such caller
(`AgentId::mcp()`); the intended embedded consumer — a harness running
several agents with different effective working directories — is many, and
they share servers instead of each launching a set.

A call arrives at a tool, which acquires the session's workspace for the
whole call, resolves its target to one *identity* through the shared
resolution pipeline, queries the language server that owns that identity, and
renders the answer as text a model reads.

```text
MCP front end            embedded agents
  AgentId("mcp")           AgentId(<agent>)
        \                     /
            Host  (Arc)                     workspaces by canonical root
              |
            Entry            one root: slots, interested agents, lifetime
              |
          Workspace          the root, one slot per configured adapter,
              |              the running server for each
          LspServer          documents owned, LSP spoken, adapter consulted
              |
         ServerHandle        three threads, one child process
```

## The governing rule

**Correct and current information beats response time.** The calling agent
has no sense of wall-clock time; the human cares that the answer is right.
Every other invariant here is a consequence, and they are stated in full in
[correctness.md](correctness.md):

1. **No timer decides an answer.** Before any semantic query the server must
   have *no outstanding work* — every `$/progress` token ended and the
   adapter's readiness policy agreeing — including work unrelated to this
   query. The only timer is an **inactivity backstop** for a broken server,
   and a silent server is probed with `$/structural/ping` before it can trip.
   No settle sleeps, no retry loops, no partial answers on a timer.
2. **Freshness comes from message ordering, not timing.** Files structural
   has touched are owned and re-pushed from disk before the query that reads
   them, and a server must apply `didChange` before it handles a later
   request on the same wire.
3. **Every range a server returns is checked against the text just read.** A
   mismatch is an error, never a retry and never an answer served from a
   stale range.
4. **Nothing is silently picked.** Several candidates are listed with their
   kinds, paths and signatures; ambiguity is an answer, not a coin flip.
5. **Every byte is paid for on every call.** One line per entry in a listing;
   caveats live in the tool descriptions once, not in every result.

## The map

| Subsystem | Doc | Governing modules |
| --- | --- | --- |
| The correctness contract every other doc inherits | [correctness.md](correctness.md) | `src/lsp/server.rs`, `src/lsp/handle.rs`, `src/tools/resolution.rs` |
| Host, workspaces, sessions, slots — who owns a server and who may use it | [ownership.md](ownership.md) | `src/host.rs`, `src/host/launch.rs`, `src/host/lifetime.rs`, `src/workspace.rs`, `src/workspace/slot.rs`, `src/session.rs` |
| Shutdown — soft admission, force reach, process trees, signals and EOF | [shutdown.md](shutdown.md) | `src/host.rs`, `src/host/launch.rs`, `src/host/lifetime.rs`, `src/lsp/handle/owner/lifecycle.rs`, `src/main.rs` |
| The transport — one server on three threads, the backstop, the probe, the bounds | [transport.md](transport.md) | `src/lsp/handle.rs` (+ `src/lsp/handle/`), `src/lsp/framing.rs`, `src/lsp/jsonrpc.rs`, `src/lsp/handlers.rs` |
| Servers and adapters — what each language server needs said to it, and `structural.toml` | [servers.md](servers.md) | `src/lsp/adapter.rs` (+ `src/lsp/adapter/`), `src/lsp/server.rs`, `src/config.rs` |
| Resolution — a name or an anchor to one identity, or a listing that says why not | [resolution.md](resolution.md) | `src/tools/resolution.rs` (+ `src/tools/resolution/`), `src/lsp/definition.rs`, `src/lsp/anchor.rs`, `src/lsp/shape.rs` |
| The three tools — `definition`, `references`, `rename`, their views, and how answers render | [tools.md](tools.md) | `src/tools.rs`, `src/tools/definition.rs`, `src/tools/references.rs`, `src/tools/rename.rs`, `src/tools/listing.rs`, `src/tools/hits.rs` |
| Discovery — where a project begins, what a tree is, and which servers it calls for | [discovery.md](discovery.md) | `src/discovery.rs` (+ `src/discovery/`) |
| The workspace's event source — what a batch means, and the thread that turns one into a refresh | [observation.md](observation.md) | `src/host/observation.rs` |
| The catalogue's measured set, order, and cuts — which server per language, and what was left out | [catalogue.md](catalogue.md) | `src/discovery/catalogue.rs` (+ `generated.rs`), `src/bin/catalogue.rs`, `tests/conformance/` |
| The raw-LSP spike — the dev instrument that dumps unrendered server answers | [spike.md](spike.md) | `src/bin/spike/` |

## Repository layout

One crate, one repository. `src/lib.rs` is the library surface the
in-process tests use — the same tools and state the binary serves, without
the binary's stdio and argv; `src/main.rs` is the front end.
`src/bin/spike/` is a separate binary, read-only by construction, kept as a
diagnostic instrument ([spike.md](spike.md)).

Where a module has grown a directory beside it (`src/lsp/handle/`,
`src/lsp/adapter/`, `src/tools/definition/`, `src/tools/resolution/`,
`src/workspace/`, `src/host/`, `src/discovery/`), the `.rs` file holds that subsystem's
vocabulary and public surface and the directory holds one file per concern.
Tests live in a sibling module — `foo.rs` + `foo/tests.rs` — so the module
path is unchanged.

## Conventions for this tree

- **Present tense.** Describe what *is*, not what landed when. The dated
  narrative is [`../journal/`](../journal/); the shipped list is
  [`../CHANGELOG.md`](../CHANGELOG.md).
- **No file:line citations** — they rot. Name the governing module inline
  ("in `src/workspace/slot.rs`") and let the code carry the detail.
- **Depth bar:** a sufficiently sophisticated peer should be able to write a
  rough approximation of the project from these docs alone.
- **Keep rationale only where it earns its place** — when it protects an
  invariant a future reader could unknowingly break, or records a road taken
  and rejected. No decisions log.
- **Describe, don't evaluate.** State mechanisms and what breaks if they
  change. No praise words and no comparative claims about other tools; assume
  any sentence may be quoted out of context by a future summarizer.
- A doc graduates from `topic.md` to `topic/README.md` + children only when
  it is too big to scan in one sitting.
