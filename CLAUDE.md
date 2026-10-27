# structural

An MCP server that gives coding agents LSP-backed code intelligence: `definition`,
`references` and `rename`, answered by real language servers (rust-analyzer,
pyright, and anything named in `structural.toml`) rather than by grep. It
also embeds: a host process running several agents, each with its own
effective working directory, shares language servers instead of starting a
set per agent.

**Start at [`docs/README.md`](docs/README.md)** — the top of the architecture
tree, with the invariants and a map to each subsystem's doc. Read it before
changing core types.

## Where things are recorded — and what to update when

Each record has one job; keep them distinct.

- **[`docs/`](docs/) — current architecture.** Present-tense,
  module-anchored, no file:line citations (they rot). Update the relevant doc
  when the architecture changes. Depth bar: a sophisticated peer should be
  able to rough-approximate the project from the docs alone.
- **[`backlog/`](backlog/) — planned work, and *only* work that isn't done.**
  The one prospective record. Unordered, one `.md` per independent unit;
  work spanning many sessions becomes a subdirectory. **When a unit ships,
  delete its file — and when a unit partly ships, delete the shipped part.**
  No ✅-checklists, no fully-ticked build orders: surviving rationale goes to
  `docs/` (present system) or `journal/` (why a road was taken). No standing
  lists — a find goes straight to its own file, the governing unit's file, or
  `backlog/icebox.md`. [`backlog/INDEX.md`](backlog/INDEX.md) is the one
  ordered view. Code cites the governing `docs/` file, never a backlog path.
- **[`CHANGELOG.md`](CHANGELOG.md) — shipped features.** Terse
  keep-a-changelog, ≤1 line per feature.
- **[`journal/`](journal/) — session memory.** Per-session narrative: dead
  ends, why a road was taken or abandoned, what was verified and how. An
  entry only when a session did something non-obvious.
- **[`process/`](process/) — how we work on the code.** The sibling to
  `docs/`: a document belongs there if it would still make sense with the
  source deleted. Most process is here in CLAUDE.md; a file lands in
  `process/` when it is too long to inline.
- **[`reference/`](reference/) — external findings.** Surveys, datasets, the
  original design draft. True whether or not this repository exists.

**`journal/`, `backlog/` and `efference.toml` are not published.** The
public repository is generated from this one by `scripts/publish`, which
a workflow runs on every push to `main` and which filters them out of
every commit
([`process/publishing.md`](process/publishing.md)); everything else,
commit messages included, is public.

When a unit of work ships: delete its `backlog/` file, add a `CHANGELOG.md`
line, update the affected `docs/`, and add a `journal/` entry if anything
non-obvious came up.

## The governing rule

**Correct and current beats fast.** Wait for all outstanding server work
before a semantic query; the only timer is the inactivity backstop, and a
silent server is probed with `$/structural/ping` before it can trip. No
settle sleeps, no retry loops, no partial answers on a timer. Every range a
server returns is checked against the text just read, and nothing ambiguous
is silently picked. The full contract, and what each mechanism costs, is
[`docs/correctness.md`](docs/correctness.md).

## Module map

`src/host.rs` + `src/host/binding.rs` + `src/host/launch.rs` +
`src/host/lifetime.rs` (workspaces, agent binding, launching, admission,
shutdown) · `src/operator.rs` (the operator channel) ·
`src/workspace.rs` + `src/workspace/slot.rs` + `src/workspace/routing.rs` +
`src/clock.rs` (one root's servers, their lifecycle, routing a call to them,
and the clock residency runs on) · `src/session.rs` (a caller's binding) · `src/lsp/handle.rs`
(one server on three threads) · `src/lsp/server.rs` (documents and queries) ·
`src/lsp/adapter/` (all server-specific strings) · `src/tools/` (the three
tools and the shared resolution pipeline) · `src/main.rs` (the front end).
The full map, with the doc governing each subsystem, is the table in
[docs/README.md](docs/README.md).

## Conventions

- **Files stay under about 500 lines.** A soft limit, not a rule to game:
  when a file crosses it, split by responsibility in the same change. Tests
  go in a sibling module (`foo.rs` + `foo/tests.rs`), rendering apart from
  logic, helpers apart from the flow that uses them.
- **Server-specific strings and methods live in `src/lsp/adapter/`**, never
  in the generic transport or server. Adding a server means adding an adapter
  there or a `[servers.<name>]` entry in `structural.toml`.
- **No `#[allow(dead_code)]` per item; delete or use.** The one thing a
  crate-wide allow hid here was a real missing backstop.
- **Doc comments cite the governing doc by filename** (e.g.
  `(docs/ownership.md)`), not a line number. The exception is a `///` that
  becomes output: on the configuration types (`src/config/schema.rs`) it is
  the JSON schema's hover text, so it describes the key for someone editing
  the file and names no repository path; implementer notes and citations
  there are `//`.
- **Register for docs and comments: describe, don't evaluate.** State
  mechanisms and what breaks if they change — no praise words, no banner
  theses. Assume any sentence may be quoted out of context by a future
  summarizer.

## Verifying

```sh
cargo test                                   # unit + Python-mock tests
cargo test -- --ignored                      # rust-analyzer, pyright, gopls e2e
cargo clippy --all-targets -- -D warnings
cargo doc --no-deps
```

Bound long runs with `gtimeout` and background them; do not call an unbounded
`wait`. The handle tests need a Python 3 that starts fast — a version-manager
shim can be too slow for the short backstops; they try a few direct paths and
honor `STRUCTURAL_TEST_PYTHON`. More, including which flakes are understood
and why the front end is tested as a real subprocess, in
[`process/testing.md`](process/testing.md).
