# Resolution: from a target to one identity

Every tool shares one pipeline: a *target* — a name, or an anchor quoted from
a file — becomes one **identity**, or a listing that says why there is not
one. Implemented in `src/tools/resolution.rs` (+ `resolution/qualify.rs`,
`resolution/anchored.rs`), over the pure helpers in `src/lsp/definition.rs`,
`src/lsp/anchor.rs`, `src/lsp/outline.rs` and `src/lsp/shape.rs`.

## Identity, not names

`workspace/symbol` only **nominates**. The pipeline:

1. **Sync** every owned document from disk.
2. **Wait** until each queried server has no outstanding work.
3. **Nominate** — `workspace/symbol` on the bare name, through the adapter's
   query rewriting. Candidates are partitioned into exact / fuzzy /
   ambiguous; only exact-name matches can resolve.
4. **Navigate** — every exact candidate's file is synced, its range checked
   against the text just read, and `textDocument/definition` asked at it.
5. **Group by target.** Candidates that navigate to the same place are one
   identity; candidates that navigate to different places are ambiguous and
   are listed with kinds, paths and signatures. Nothing is picked.

`parse_definition_result` normalizes defensively: `Location`,
`Location[]`, `LocationLink[]`, `LocationLink`, `null` and `[]` are all shapes
the spec allows, and all are handled.

Which servers are asked: a `path` naming a file goes to the server for that
file's language; otherwise every running server is asked and the candidates
are merged, with cross-server collisions tagged by the server that owns each.

## Qualified names

`Config::new`, `Config.doubled`, `pkg/Greet`. The **last** segment is the
bare name sent to the index; the earlier segments are a *filter* over the
exact candidates by the `containerName` the server reported — not
language-level path resolution. rust-analyzer and pyright report the impl or
class type as a method's container (`new` → `Config`) and report **nothing**
for a free function, so a module-path qualifier (`project::project_with`)
cannot resolve (`../backlog/module-path-qualifiers.md`). A qualifier matches
when the container's own segments end with it.

Because the qualifier filters a pool it did not widen, a common method name
whose pool is capped by the server's fuzzy search can fail to resolve even
though the symbol exists (`../backlog/file-scoped-lookup.md`).

## Anchors

An anchor is source text quoted verbatim from `path`, and `focus` is the
token within it that is meant. `src/lsp/anchor.rs` is a pure function over
the text read from disk: the anchor must occur **exactly once** in the file
and the focus **exactly once** within the anchor, and zero or several
occurrences are errors that say where the occurrences are. The resulting byte
range is converted to a position with the server's encoding.

Anchors skip the symbol index entirely: the file is synced, then
`textDocument/definition` is asked at the focus. This is what reaches
locals, parameters, call sites and expressions — everything a name cannot
address.

Focus matching is plain substring matching today, so a focus that is a
prefix or suffix of a neighbouring identifier collides
(`../backlog/focus-identifier-boundaries.md`).

## The listings

When there is no single identity, the answer is a listing — the same
rendering for every tool (`src/tools/listing.rs`), one line per entry:

- **not found** — the name matched nothing exactly, with any exclusion notes;
- **fuzzy** — "similar:" candidates with their kinds and locations, never a
  pick;
- **unqualified** — the name resolved but no candidate sits in a container
  matching the qualifier, and the bare candidates are listed;
- **ambiguous** — several distinct definitions with kind, path and signature;
- **null definition** — the index nominated something the server will not
  navigate to.

## Shaping the answer

The item's **extent** comes from the server's `documentSymbol`:
`innermost_at` picks the smallest symbol whose range contains the position,
handling both the hierarchical `DocumentSymbol[]` and the flat
`SymbolInformation[]` shapes. `chain_at` gives the enclosing chain, which is
what names an item in a hit line and what the test heuristic reads.

Tree-sitter then **decorates**, it does not resolve: `extend_start` widens
the extent backwards over the leading doc comments and outer attributes
(Rust) or decorators (Python) that belong to the item — the decorated-unit
rule. A language with no grammar keeps the server's extent unchanged.
`item_at` is the fallback for a Rust server that gives no extent at all.
Selection semantics: the smallest enclosing **named** item wins — a position
in a method's body yields the method, not its `impl`. When no item contains
the position, callers must fall back to the server's reported range
explicitly rather than guessing.

## Test classification

`src/tools/classify.rs` decides whether a hit is test code, from the outline
chain and the path: a file or directory named for tests, or a module named
`tests`. It is a **heuristic and says so** — an inline `#[cfg(test)]` block
under another name is listed as production. It also produces the enclosing
item label every hit line carries.
