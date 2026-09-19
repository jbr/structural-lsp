# Discovery: what a tree is, and which servers it calls for

Four layers, in `src/discovery/`, all of them pure functions over their
inputs. Nothing here starts a process, watches a filesystem, or takes a
lock — the whole subsystem returns values, and those values are what the
tests assert.

| layer | module | what it produces |
| --- | --- | --- |
| roots | `src/discovery/roots.rs` | where a project begins, and why |
| evidence | `src/discovery/evidence.rs` | the markers above a directory and the sources within it |
| catalogue | `src/discovery/catalogue.rs` | what a server is, what it serves, how it ranks |
| selection | `src/discovery/selection.rs` | plans, each naming a server and its reasons |

Acquisition is connected. `Workspace::new` lays out one `ServerSlot` per
plan rather than one per configured adapter (`ownership.md`), so a server
no evidence calls for is never warmed and never appears in a message; and
`Host::agent_started` keys a workspace on the project root the walk found
rather than on the working directory it was handed, so two agents in
sibling members of one cargo workspace share one server. What remains of
`backlog/discovery/selection.md` is the generated catalogue and the
fallback ladder.

## The root is the key, and only the key

`Workspace::root` is one path doing three unrelated jobs, and discovery
separates them because the right answer differs for each:

1. **What a server is told at startup.** Largely the server's own problem:
   rust-analyzer and gopls both locate the enclosing project from a
   directory well inside it, and answer about files outside the directory
   they were given (`reference/server-root-discovery.md`). A server that
   does not walk up is a fact about that server, and belongs in its
   catalogue entry.
2. **What answers are filtered against.** The caller's working directory,
   deliberately: an agent working in `crates/alpha` is asking about
   `crates/alpha`. `out_of_root` and `out_of_scope` already do this and
   already say what they removed (`resolution.md`).
3. **What the host keys a workspace by.** The sharing point, and what
   `roots` decides.

Because a key is only ever compared against another key, a key that comes
out wrong **costs sharing and never costs correctness** — what a caller is
told is filtered by job 2, which does not consult the key. That asymmetry
is why the rule can be simple.

### The rule

`Ancestry::gather` walks up from the working directory, bounded by the
nearest version-control root, collecting `PROJECT_MARKERS`. `root()` takes
the **outermost** marker in that span. No marker, or no repository: the
working directory is its own root.

- **Outermost, not nearest.** The nearest `Cargo.toml` above a workspace
  member is the member's, which keys sibling members apart and gives each
  its own server indexing the same tree. The outermost is the workspace —
  the boundary rust-analyzer chose for itself when started inside a
  member. This is Helix's convention rather than nvim-lspconfig's.
- **The VCS root bounds the search and is not the answer.** A repository
  holding `backend/Cargo.toml` and `tools/Cargo.toml` and no manifest of
  its own has two project roots. Answering with the repository would fuse
  unrelated projects into one server told to index everything.
- **The nearest repository wins,** so a submodule or nested checkout is
  its own span.

`Root::reason` carries which of these applied, and `explain()` renders it
as a line. `host::workspace_key` is what calls it.

### The key and the view

Separating them is what lets sharing widen without answers widening. A
`Session` carries the working directory it was bound with as its **view**,
and that is what every user-facing path is relative to: `path` joins onto
it, references outside it are filtered and counted, and locations are
rendered relative to it. The workspace key is only what servers are shared
under and what they are told at startup.

So two agents in `crates/alpha` and `crates/beta` hold one workspace and
one server, and each is still answered about its own member. `Target`
carries the view, which is how it reaches resolution and the views without
any tool reaching for the key.

One consequence, decided rather than discovered: `structural.toml` is read
from the **project root**, since that is the workspace root. Configuration
comes from the project rather than from whichever directory an agent
happened to start in.

## Evidence is evidence, not proof

`Evidence::gather` pairs the ancestry with a downward scan of the root it
implies. The scan starts at the **root**, not the working directory: a
sibling member's sources belong to the same project.

`Sources` counts extensions, skipping hidden directories and
`SKIPPED_DIRECTORIES` — build output and dependency trees describe a
toolchain's bookkeeping rather than what a project is written in, and
`node_modules` alone would make almost any tree look like JavaScript. The
scan stops at `FILE_BUDGET` files and records that it did, because an
extension absent from a partial scan is not evidence of absence.

A marker says a project boundary is here. It does not say that any
particular server applies, or that its command exists. Evidence yields
candidates and reasons; the catalogue decides what to do with them.

## The catalogue ranks by what a server answered

A catalogue `Entry` is data — name, command, the languages and extensions
it serves, its rank, and its provenance. Adding a server is an entry, not
a build; `Adapter` (`servers.md`) remains only for protocol behaviour that
differs, and most entries need none.

Rank is a **measured score**: what the server answered when asked the
questions `show`, `uses` and `action` ask, through structural's own tool
layer (`tests/conformance/`). Breadth of advertised capability is not the
metric and neither is how many editors ship a server — pylsp advertises
more than pyright and is worse on this path.

The order is total and stated, because determinism is asserted against a
stated order rather than against a second run:

1. score answered, descending;
2. editor consensus, descending — the one job consensus has, standing in
   for maintained-and-preferred among servers that answered equally well;
3. name, ascending.

An unmeasured entry ranks as zero answered. That is not a claim it is bad:
the premise is preferring what was verified, and the way to promote a
server is to measure it. It still wins when it is the only candidate.

There is **no admission threshold.** A bar would fire on a server that
initializes and serves nothing, whose causes are a bug of ours or a
project the operator never set up, and removing the only candidate for a
language improves neither.

`Score` cannot be constructed without a `Measurement` — server version,
structural revision, fixture revision, date. A score measures the pair:
four of the defects the first conformance run surfaced were structural's,
and one server went from two of seven to five of seven when this client
stopped misreading a legal range.

The shipped catalogue carries no scores. Producing them, and the modest
verified set they rank, is `backlog/discovery/catalogue-synthesis.md`.

## Selection is a function, not a side effect

`select(evidence, catalogue, overrides) -> Selection`. It returns plans
and declines, and does nothing else.

A `Plan` names a server, the language it was chosen for, the root it would
start against, its command, the alternatives in rank order, and `why` — a
list of `Reason` values, not prose, so a test can assert a plan is right
for the *right* reason. A `Declined` records a catalogue entry no plan
names and why: no evidence for its languages, disabled, or outranked.

Precedence is explicit and ordered: an **override** beats the
**catalogue**, which beats a **guess from evidence**. `structural.toml` is
that layer (`servers.md`): naming a server configures it, `disabled = true`
takes it out of consideration — built-in or not, which configuration could
not express before — and a name the catalogue does not know is added
generically. A disabled entry is never validated, because `disabled = true`
on a server with no `command` is a statement about the project rather than
an incomplete definition.

Two reasons are kept apart on purpose. `HighestScore` means it outranked
named alternatives; `OnlyCandidate` means there was no one else. On a
machine with one server per language, which is the common case, only the
second is defensible. Its acquisition-time twin — the only candidate whose
command would actually start — arrives with fallback.

A plan is a **proposal**. Whether a server can start is acquisition's to
discover, and a failure there is reported rather than dropped.

Plans come out sorted by language, then by server. Every collection the
module builds is ordered before it is read, because purity means the
nondeterminism that can reach selection arrives with its inputs —
directory iteration order, and any map whose iteration order is not fixed.
