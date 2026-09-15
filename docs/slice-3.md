# Slice 3: `uses`

Plan written 2026-09-14 after slice 2 (`docs/slice-2-handoff.md`). `uses`
is the second tool: references to a resolved symbol, with one line of
context per hit, grouped by file and enclosing item, tests set apart. The
governing rule of `docs/slice-2.md` applies unchanged; nothing here adds a
timer, a retry, or a server string outside `src/lsp/adapter/`.

## What it does

`uses(symbol, path?)` runs `show`'s pipeline up to identity, then asks the
identity's server for `textDocument/references` at the definition target,
and renders the hits.

1. Resolve exactly as `show` does (`src/tools/resolution.rs`, shared): sync
   owned documents, wait, `workspace/symbol`, sync and range-check each
   exact hit, wait, `definition`, group by target. Not found, fuzzy-only and
   ambiguous answers are the same listings `show` gives, from the same
   rendering (`src/tools/listing.rs`).
2. One identity: sync its target file, check the name sits at the target
   range, wait, `textDocument/references` at the target with
   `includeDeclaration: false`. Any hit that still equals the target range
   is dropped (a server that ignores the flag).
3. For every file with hits: read it from disk (**not** synced: a synced
   file is owned for the rest of the session and re-fed before every later
   query, and a broad `uses` on a large tree would own hundreds), check
   that each hit's range holds the symbol's name or one of the adapter's
   reference aliases (`Self` for rust-analyzer; the spike's raw capture
   shows `-> Self` and `Self { .. }` reported as references to the struct).
   A hit that holds anything else means the server's view of that file
   disagrees with disk: an error naming the file and line, never a listing
   built on it. Then `documentSymbol` for the enclosing item chain
   (`outline::chain_at`; rust-analyzer, pyright and gopls all answer it for
   a file the client never opened), and the hit's source line from the
   text just read. Only the definition's file is owned, as with `show`.
4. Hits outside the workspace root are counted and excluded, like `show`'s
   candidates. `path` keeps `show`'s meaning: a scope for *resolving* the
   symbol (and the server pick); it does not filter the references. A
   separate filter for "uses within this directory" is a later addition if
   asked for.

## Output

```text
fn choose · src/lib.rs:43 · 2 references · 2 files
src/lib.rs
  58 · run · let offset = choose(Mode::Fast);
src/other.rs
  12 · Other::go · choose(mode)
tests (heuristic) · 1 reference
src/lib.rs
  66 · tests::smoke · assert_eq!(choose(Mode::Fast), 1);
```

- Header: `show`'s header for the definition, then the counts. References
  and files are counted separately; no "total" is claimed. Zero hits: `no
  references`.
- One line per file (root-relative), then one line per hit: 1-based line,
  the enclosing item's name (innermost, with its container chain joined by
  `::` when nested, e.g. `tests::smoke`; omitted when the hit is outside
  every item), and the source line trimmed. The line number and the file
  header together give `path:line`.
- Tests are a trailing section, labelled heuristic: a hit is a test when its
  enclosing chain contains a module named `tests` or `test`, or its path has
  a `tests`/`test` directory component, or its file is named `tests.<ext>`
  (the sibling-module convention), `test_*` or `*_test.<ext>`/`*.test.<ext>`.
- Notes only when degraded: references excluded outside the root; hits
  beyond the listing cap (100 hits; the rest reported as a count per
  section); a file whose outline was unavailable (its hits carry no item).
- The tool description and MCP `instructions` carry the caveats once.

## Code

- `src/tools/resolution.rs`: the shared pipeline, moved out of `show.rs`
  (`discover`, `definition_of`, `ambiguous_entries`, `signature_of`, the
  scope and symbol validation). `show.rs` keeps the body path.
- `src/tools/listing.rs`: the shared listing renderers.
- `src/lsp/references.rs`: pure normalisation of the `references` result
  and grouping by file, with tests.
- `src/lsp/outline.rs`: `chain_at`, the containing-symbol chain.
- `src/lsp/server.rs`: `references()`.
- `src/lsp/adapter.rs`: `reference_aliases()`, default empty; rust-analyzer
  returns `Self`.
- `src/tools/uses.rs` + `uses/render.rs` + `uses/tests.rs`.

## Verification

- Unit: references parsing/grouping; the test heuristic; rendering with
  fake hits (byte-exact).
- Gated (`cargo test -- --ignored`): rust fixture `uses choose` and `uses
  Config` (the `Self` hits verify); python fixture `uses choose` through
  pyright (the import and the call); the two-server root (ambiguous
  listing, scoped resolution); gopls through `structural.toml` with a
  second file calling `Greet`.
- Smokes: `tools/list` now names `show` and `uses`.

## Not in this slice

`action`, `inspect`, `outline`, anchor/focus targeting, diagnostics,
`didSave`, a reference scope filter, pagination beyond the cap, and any
bound on the number of owned documents (every file with a hit becomes
owned; watch the count on large results and cap it if a server minds).

## Done 2026-09-14, deviations from the plan above

- The shared pipeline is `src/tools/resolution.rs` (`resolve_symbol`),
  not `resolve.rs`: a module and function named `resolve` made this
  repository's `show resolve` (the identity test) ambiguous with the
  function in `definition.rs`.
- pyright's `documentSymbol` nests parameters and locals under their
  function as `Variable` children, so the innermost symbol at a hit inside
  `def __init__(self, limit=DEFAULT_LIMIT)` was the parameter. The rule is
  structural, not by kind alone: a value-kind symbol (variable, constant,
  field, property, enum member, type parameter, literals) whose parent is
  a function, method or constructor is skipped; anywhere else it is the
  item, so a Rust `const`/`static` (also constant/variable to
  rust-analyzer) at module level keeps its name. `uses/classify.rs`.
- Hit files are read from disk, not synced (review point from
  structural-d1; the plan above said "sync it" and was wrong about the
  cost). The freshness check is unchanged.
- The test heuristic is stated once, in the tool description, including
  what it cannot see: an inline `#[cfg(test)]` block under a name other
  than `tests`/`test` is listed as production.
- rust-analyzer's `use` paths are hits too (`pub use super::Config`, and a
  multi-line `use crate::x::{...}` list, whose hit line is the continuation
  line); they are listed with no item when they sit outside every symbol.
- `tests.rs` counts as a test file: this repository's own sibling-module
  convention put `definition/tests.rs` in production until it did.
- Output as measured: fixture `uses choose` is the header, one file line
  and one hit line, 89 bytes; `uses Config` lists six hits including the
  two `Self` occurrences inside the impl.
- Every gated test passed on the first full run after those fixes:
  rust-analyzer (fixture, scratch copies, this repository), pyright, gopls.

## For the next person

The identity test (`tests/mcp_smoke/identity.rs`) runs against this
repository, so the repository's own symbol names are load-bearing: adding
a `mod resolve` made `show resolve` ambiguous and the module had to be
renamed. Anything else that runs against the repo inherits that. A scratch
fixture with deliberately chosen names would be sturdier; do that before
adding a third such test rather than renaming code around the suite.
