# User-testing the LSP tools: first outside use

Report written 2026-09-14. The first session to use the three tools
(`show`, `uses`, `action`) that was not also developing them. The tools were
driven over the MCP mount from a working session in a real repository
(`../efference`, 39 modules, ~16k lines of Rust), not on the fixture.

**Provenance.** `structural` at `5bc2ef1` (slice 4, item 3 — `action`/rename).
Servers: rust-analyzer 1.100.0-nightly (`5db7f4be8`, 2026-09-01), pyright
1.1.403. Target repo `efference` at `5bf8525`. The target working tree was
left clean: the one rename was reverted with `git checkout`, and the one
probe symbol appended to test freshness was removed.

**How it was driven.** Definitions and all eight views; `uses` from bare
names, qualified names, anchors, and a local variable; `action` preview and a
real apply-then-revert; Rust and Python; core types down to locals; and an
assortment of malformed inputs (no target, unknown symbol, bad view, glob
path, nonexistent path, path escaping the root, ambiguous names). "Ratings"
below are one user's judgment, not measurements.

**Verdict.** The design's hardest promises hold: nothing is ever silently
picked, errors are specific and asserted-looking, `related-tests` hands back a
runnable command that actually runs, and rename is safe enough to use.
Three findings are correctness bugs, and one of them breaks the headline
feature (`symbol = "Type::method"`) in exactly the common-name regime it was
built for. The rest are ergonomic gaps that matter more as the target repo
grows.

---

## Correctness

### C1. Qualified lookup fails for common method names on a real tree — the feature's own acceptance case

`symbol = "Type::method"` is slice 4's headline item, with `Config::new` named
as its `done when`. On the real repo:

```
show symbol=ToolCall::new   → no 'new' is in a container ending in `ToolCall`;
                              the 'new' candidates: variable new ·
                              scripts/migrate-assistant-output.py:34
show symbol=RawResult::text → same shape
show symbol=new             → variable new · scripts/migrate-assistant-output.py:34
```

but the unique-named siblings on the same types resolve fine:

```
ToolCall::with_intent      → fn · src/log.rs:568
RawResult::with_images     → fn · src/log.rs:644
Config::from_toml          → fn · src/config.rs:376
```

The bare-name listing pins the mechanism: **`ToolCall::new` is not in the
candidate pool at all.** The qualified path is "a filter over the same exact
candidates" (slice 4, item 1), and when `workspace/symbol` for `new` returns a
short, capped list — here one Python variable — the filter has nothing to
narrow to. This is plausibly a result cap on the server side (rust-analyzer's
symbol search is fuzzy and bounded); the important part is the consequence,
not the cap.

Scoping `path` does **not** rescue it:

```
show symbol=new path=src/log.rs          → no symbol named 'new'
show symbol=ToolCall::new path=src/log.rs → no symbol named 'ToolCall::new'
```

So the `path` argument narrows after the pool is already thin, rather than
searching the named file's own symbols. A `path` that names a file is the
natural request to answer from that file's `documentSymbol` tree, which would
find `ToolCall::new` deterministically and sidestep the workspace-wide cap.

Why the fixture test passes and this fails: the `done when` runs `Config::new`
on the rust fixture, where the candidate list is small enough that the target
survives whatever bounding happens. The case that breaks is the one the
feature exists for — `new`, `text`, `sync`, `execute` on a tree with many of
them. The acceptance test does not exercise that regime.

### C2. A bare-name query can be stale; only `path`-scoped queries are current

The tool description says the answer is **current**: "owned files are
re-synced from disk and the servers are waited for before every query." With a
freshly written symbol:

```
append `pub fn lsp_probe_marker_xyz() -> u32 { 42 }` to src/pretty.rs
show symbol=lsp_probe_marker_xyz            → no symbol named '…'   (also after a 4s pause)
show path=src/pretty.rs view=outline        → fn lsp_probe_marker_xyz · 142   (seen)
show symbol=lsp_probe_marker_xyz path=src/pretty.rs → fn · 142   (seen)
```

The per-file paths are current; the bare workspace-symbol query is not, and
does not become so on its own. The "owned file" re-sync is real, but a bare
name query owns no file, so the one path an agent reaches for first is the one
that can miss a recent edit. `structural/CLAUDE.md`'s "correct and current
beats fast" is the right rule; this is a case where current is not delivered
and the failure is silent — the honest-looking answer is "no such symbol."

### C3. Rename does not rewrite intra-doc links

Applying `rename project_with → project_with_placement` (13 edits, 4 files)
rewrote the definition, the imports, and every call site in source and tests,
correctly, and left exactly one stale reference:

```
src/project.rs:94:  /// [`project_with`] at the default (Anthropic-native) image placement.
```

Code is clean; the doc link now dangles. One line per rename, and it is the
kind of residue a `cargo doc` run flags and a reader trips on. Either rewrite
intra-doc links or say they were not touched.

### C4. `implementations` renders a derive as duplicated identical rows

```
show symbol=ToolOutcome view=implementations
  enum ToolOutcome · src/log.rs:942 · 5 implementations · 1 file
    940 · ToolOutcome · #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
    … (five rows, all identical, all at 940)
```

Five rows, same line, same text — there is no way to tell which derived trait
is which, so the listing conveys only "there are some derives." The same view
on `Notify` renders its derive as a single row beside its two real impls, so
the two cases disagree with each other. For `implementations` the useful
output is a per-trait row; a derive could be one row naming the traits, or the
name of the trait per row.

---

## Collisions with documented intent

These look deliberate in the slice docs. Reported because the intent and the
observed behavior are worth weighing against each other, not because the code
is wrong.

### D1. `focus` substring semantics produce false ambiguity

slice 4, item 1: "`focus` is a substring of the anchor that must occur once
within it." Counting occurrences of the substring, not the identifier:

```
uses anchor="let (boundaries, elided) = extract_boundaries(log);" focus=boundaries
  → focus `boundaries` occurs 2 times in the anchor
```

The second occurrence is inside `extract_boundaries`. `focus=elided` on the
same anchor works. Any focus that is a prefix or suffix of a neighbouring
identifier — `new` in `renew`, `config` in `config_path`, `boundaries` in
`extract_boundaries` — is blocked by a collision that the user cannot see or
correct except by quoting a different span. Substring semantics somewhere
(e.g. inside a string literal) may be wanted; identifier-boundary matching for
the common case is what an agent will hit.

### D2. `expanded` refuses a `#[derive]` with a message that says it isn't a macro

```
show anchor="#[derive(…)] pub struct ToolCall {" focus=derive view=expanded
  → not applicable: the focus is not a macro call
```

`docs/lsp-server-extensions-for-agents.md` rates `expandMacro` **High** and
names `#[derive]` first among what it reaches ("`#[derive]`, `tokio::main`,
`thiserror`"). slice 4 says "not applicable when the focus is not a macro
call," which is literally true of a derive attribute — but a reader of the
survey will expect `#[derive]` to be the flagship case. Function-like macros
do work (`format!`, `vec!` both expanded correctly). Either derive expansion
is supported and this is a bug, or the survey's example outruns the slice; the
message should say which ("derive expansion is not supported", not "not a
macro call").

### D3. Module-path qualifiers can never resolve

slice 4 documents this for free functions ("rust-analyzer reports no
`containerName` for a free function even inside `mod tests`, so `tests::smoke`
cannot be reached by qualifier"). The user-visible result is that
`project::project_with` and `semantic::project_semantic` both fail with the
lone candidate reported as "no container." Since `module::fn` is a shape an
agent will type naturally, the tool description's example (`Config::new`)
implies it works; the error could say the qualifier only reaches methods and
class members.

---

## Ergonomics and capability gaps

### E1. `uses` truncates core types arbitrarily, with no way in

`ToolCall` has 271 references across 72 files; 100 are shown. The result's own
footer:

```
note: 171 more reference(s) not shown (listing cap 100)
```

Three compounding problems:

- The cap is fixed; there is no pagination and no way to raise it.
- Ordering is alphabetical by path, so truncation drops whole alphabetical
  subtrees rather than the least relevant references. "Where is this used"
  gets a biased sample with no indication of the bias.
- `path` scopes where the *symbol* is looked up, not where references are
  listed (the documented semantics), so a request like "references within
  `src/agent/`" cannot be expressed. With the cap, this is the escape hatch
  the feature most needs.

`uses` on a core type is the flagship question for this tool; as it stands the
answer is "100 of 271, alphabetically, and you cannot narrow it."

### E2. Test/production separation appears in `uses` only

`uses` splits a "tests (heuristic)" section; `incoming-calls` and
`implementations` interleave the two with no marking. `implementations Probe`
listed the production `LiveProbe` impl and the test-only `Table` impl as
peers. Whatever distinguishes a test is already computed for `uses`; applying
it to the call-graph and implementation views (or marking rows inline) would
make all three consistent.

### E3. `outline` caps at 200 lines and cannot outline a directory

`outline src/log.rs` (232 symbols) ends "32 more symbol(s) not shown (listing
cap 200)" with no continuation. `outline` on a directory is refused ("`outline`
needs `path` to name a file"), so a module tree — the unit `lib.rs` is
organized in — cannot be surveyed in one call. The 500-line file convention
means the cap bites exactly on the files worth summarizing.

### E4. `outgoing-calls` mixes dependency noise into project callees

`outgoing-calls project_turns` returned four callees, two of them stdlib
(`unwrap`, `lock`) with absolute toolchain paths
(`/Users/…/rustlib/…/result.rs`), interleaved unseparated with the two project
callees. Half the output for a four-callee function was dependency noise; on a
longer function this is the dominant cost. Marking or grouping non-workspace
callees would fix it.

### E5. A directory `path` scope masks the unserved-language message

A file path with no server gets a specific refusal: "no running language
server serves `md` files (running: rust-analyzer, pyright)." A directory path
does not: `path=web` (TypeScript, no server) returns a plain "no symbol named
'app'" with exclusion notes. The unserved-language signal is only available
when the scope happens to be a file; a directory scope loses it.

### E6. Python outlines list every local variable

pyright's `documentSymbol` includes locals, so an outline line-per-symbol
swallows function bodies. At 17 symbols it is readable; in a large module the
Python outline will be mostly locals. Rust's outline does not have this
problem, so the two languages' outlines are not comparable in signal.

---

## What held up

Worth keeping as-is, because these are the properties that make the tools
trustworthy rather than merely useful:

- **Ambiguity is never resolved silently.** Every collision returns an
  enumerated list, and cross-server collisions are tagged with the server that
  owns each candidate (`convert`: one rust-analyzer fn, two pyright defs).
  Supplying `path` then resolves it. A bare unknown name falls back to a
  "similar:" list with kinds, not a wrong pick.
- **`related-tests` is the standout.** It returned exact module paths and
  ready-to-run commands; the command was run verbatim and passed. It is the
  one view that hands the agent its next action.
- **Anchors reach what names cannot.** `uses` on the local `elided` resolved to
  its single reference inside a function body. This is the capability that
  justifies the anchor path.
- **Rename is safe.** Preview gave per-line old→new with a digest; a wrong
  digest refused to apply and returned a fresh preview; a correct apply wrote
  all 13 edits across 4 files, re-synced, and reverted cleanly. Rename targeted
  by anchor and by name produced the identical preview and digest.
- **Validation is uniform and specific.** Missing target, unknown view (lists
  all eight), glob path, nonexistent path, and path escaping the root each get
  a distinct, stated message. Nothing crashes, nothing guesses.
- **Cross-language is real.** pyright served outline, definition, `uses`,
  rename, `incoming-calls`, and `type`; Rust-only views degrade per-server
  ("`related-tests` is not supported by pyright"). `type` via `hoverRange`
  answered a real expression type in both languages.
- **`expanded` works on true macros** (`format!`, `vec!`), which is the
  survey's actual prize.

## If I were triaging

Correctness first: **C1** (the qualified-name acceptance test needs a
common-name, real-tree case; a `path`-scoped lookup that falls back to the
file's own symbols would fix both the cap and the determinism), then **C2**
(currentness should not depend on whether `path` was passed), then **C3**
(intra-doc links) and **C4** (derive rows).

Then **E1**, which is the largest gap between what the tool is for and what it
delivers on a real repo: pagination, relevance ordering, and subtree-scoped
reference listing. Then **E2**–**E4** as normalization, **D1**–**D2** as
decisions to make explicitly rather than by accident.

None of this changes the shape of the design; the pieces are already the right
pieces. The bugs are in what the candidate pool contains and what a query
considers current, and the gaps are in how much of a large-tree answer the
tool can show.
