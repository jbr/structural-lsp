# Briefing for an outside review of unit 3

You are being asked to review a plan. This document tells you what the project
is, where the plan sits, what to read, and what is genuinely open. It is
written to be read cold: assume no prior context, and please verify claims
against the code rather than taking this document's word for them.

## What the project is

`structural` is a Rust MCP server that gives coding agents LSP-backed code
intelligence: `show`, `uses` and rename, answered by real language servers
(rust-analyzer, pyright, and anything named in `structural.toml`). It is also
meant to be embedded in a long-lived host process — an application that runs
several agents at once, each with its own effective working directory — where
those agents should *share* language servers rather than each starting a set.

## Where the work is

The governing plan is **`docs/lifecycle-and-discovery.md`** (the "parent plan",
written before any of this was implemented; read it first, and note that
source is the authority on current behaviour where it disagrees). It lays out
six sequenced units:

| Unit | What it is | State |
|---|---|---|
| 0 | record the contract, establish regressions | partly done, partly open |
| 1 | extract the shared host; preserve server selection | **implemented** (`docs/host-extraction.md`) |
| 2 | make server lifecycle explicit | **implemented** (`docs/server-lifecycle.md`) |
| 3 | discovery and selection as pure policy | **plan written, nothing built** — your review target |
| 4 | observation and incremental discovery | not started |
| 5 | connect observation to LSP synchronization | not started |
| 6 | multi-workspace MCP routing; integrate the embedded consumer | not started |

Git log for the shape: `5b3e903` (unit 1), `714b854` (unit 2's plan), `2351961`
through `8b40c9f` (unit 2's implementation), `bb2919a` (unit 3's plan).

## The review target

**`docs/discovery-and-selection.md`** — unit 3's plan. Its Done-when, quoted
from the parent plan:

- fixture trees produce deterministic plans;
- irrelevant servers do not start;
- overrides win;
- an unavailable preferred candidate can fall back to an available alternative;
- selection and project-root choices are explainable.

### The research it is built on

`docs/servers-list/` is thirteen editor inventories (Eglot, lsp-mode and its
external clients, lsp-bridge, nvim-lspconfig, Helix, vim-lsp-settings,
kakoune-lsp, Kate, OpenCode, Zed, mason-registry), extracted from source, and
merged into 2,218 normalized records. `docs/servers-list/README.md` describes
the schema and, unusually, its own confidence tiers. It is the *input* to the
catalogue, and it is an overabundance rather than a gap: the operator has
commissioned it but not yet synthesised it, and that synthesis — which servers
this project will stand behind, in what order, with which root rules — is
deliberately theirs and out of band.

## What to read in the code

Read enough to know whether the plan can be built as written. These are the
files that matter, with why:

- **`src/workspace.rs`** — a workspace is a root plus one *slot* per configured
  adapter; `serving()` is the entire routing rule today (extension → adapter),
  and `select()` answers a query from the running slots. This is what unit 3
  replaces.
- **`src/workspace/slot.rs`** — one server's lifecycle: `Cold | Starting |
  Running | Failed`, election, and the RAII `Launch` guard. Unit 3's plans
  become slots, so this is the seam.
- **`src/host.rs`** — the host: workspaces keyed by canonical root, the
  `Lifetime` (a shutdown manager plus one claim flag, shared by every entry),
  agent start/end, and the four-pass shutdown. Units 1–2's invariants live
  here.
- **`src/config.rs`** — `structural.toml`: built-ins, overrides, generic
  servers, and `default_extensions` (a catalogue in embryo, eleven language
  ids). Unit 3 makes this the *override* layer over a catalogue.
- **`src/lsp/adapter.rs`** — `Adapter`: command, languages, readiness,
  capabilities. Most catalogue entries should need no adapter.
- **`src/lsp/server.rs`** — `LspServer::spawn` then `initialize`; and
  `sync`/`sync_owned`, which is where "document state does not survive a fresh
  instance" comes from.
- **`src/session.rs`** — `acquire(scope)`: activate what a scope needs, then
  take the workspace lock. The one place the ordering is enforced.
- **`Cargo.toml`** — small dependency set; there is no async runtime.

Tests worth knowing about, because they are the instrument unit 3 has to use:
`src/host/tests.rs` (fourteen tests over a scripted Python mock that records
every launch, so "irrelevant servers do not start" is a pid assertion) and
`src/test_support.rs` (that mock). Run `cargo test`; `cargo test -- --ignored`
spawns real servers and is slow.

## Invariants you should assume hold, and may challenge

Units 1–2 established these, and unit 3 must not break them:

1. **One workspace per canonical root**, shared by callers naming the same
   root; unrelated roots do not share a process and do not serialize.
2. **A launch holds no lock while it spawns and initializes**, and publishes in
   a single workspace-lock hold. Activation happens *before* the workspace lock
   is taken, because a launch publishes under it.
3. **Stopping a server is one operation**: take it out and mark the slot cold —
   used by shutdown, and by eviction in unit 4's predecessor.
4. **Shutdown's disjunction**: every spawned child is either published and
   stopped by the sweep, or killed by the launcher that spawned it. The claim
   is set before the sweep and re-read under the workspace lock before
   publishing.
5. **Nothing proposes a plan that cannot say why.** Every answer in this
   project carries its reasons; the research's confidence labels exist so a
   heuristic cannot silently become a fact.

## What is genuinely open

**Open-ended, and the main event.** Is unit 3's plan the right shape? Where
would you have done it differently *structurally* — not cosmetically? Where is
it over-built for its Done-when, and where under-built? What question does it
never ask? And above all: **what would make it falsifiable?** The plan contains
a table mapping each Done-when clause to something that could fail, and two
admitted limits (that "irrelevant servers do not start" is only assertable
because launches are recorded, and that a fixture tree can only show the rules
do what they say on trees we chose). Attack that table: is any row actually
unfalsifiable, and is any clause missing from it?

**Second, and separable.** A unit-2 mechanism question, marked so it does not
contaminate the framing review. At publish time a launcher asks *"is the host
shutting down?"* — a single claim flag, shared by every slot. A reviewer has
argued it should instead ask *"is this slot still the slot I was starting a
server for?"*, with a per-slot generation counter bumped whenever the slot is
stopped, carried in the guard, and compared at publish; that would subsume the
flag read at publish time, and would matter when eviction arrives (a launch
that publishes after an eviction undoes it). I could not reconstruct the exact
interleaving that makes this bite, and I have been wrong twice in exactly this
part of the design, so: is the generation counter the right mechanism, and is
my inability to construct the interleaving evidence that it is unnecessary?

## How to answer

- **Write your work product to disk** at `docs/astra-review-unit-3.md`, not
  only in conversation, so it survives and can be committed and referred to
  later. Structure it however you think is right; ranked findings, a rewritten
  section, a critique of the shape — whatever serves.
- **You are welcome to rewrite or edit the plan itself**, not only comment on
  it. If the right answer is a different decomposition, write it. But **talk to
  me first** — message me before rewriting anything, so we can disagree
  properly rather than trading documents. If the sessions tools work for you,
  use them; if they do not, say so in your answer and I will read your
  transcript. Do not spend effort fighting the channel.
- **Do not consolidate your context.** I expect to want more than one exchange.
- Cite files and lines where you make claims about the code, and say when you
  are giving a *preference* rather than deriving something from evidence. That
  distinction is the whole reason for asking someone with a different read.
