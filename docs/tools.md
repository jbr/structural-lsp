# The tools

Three MCP tools, declared in `src/tools.rs`: `show`, `uses`, `action`. The
same three run from argv (`structural show choose`), since mcplease owns
both dispatch paths. All three take their target the same way and share the
resolution pipeline ([resolution.md](resolution.md)), so a target that
`show` would list as ambiguous is listed identically by the other two.

## Targeting, common to all three

| argument | meaning |
| --- | --- |
| `symbol` | the name, bare (`choose`) or qualified by its container (`Config::new`, `Config.doubled`). Exact match on the name. |
| `path` | a file or directory under the root, relative to it. A file nominates `symbol` from that file's own outline rather than the workspace index, and picks its language server; a directory narrows the index. With `anchor`, the file the quoted text is in. |
| `anchor` | source text quoted verbatim from `path`, occurring exactly once in it — the way to reach a local, a call site, an expression. |
| `focus` | the token within `anchor` that is meant; the whole anchor when absent. |

The workspace root is fixed at server start (cwd, or `STRUCTURAL_ROOT`).
`path` only scopes inside it: it is canonicalized, and a symlink escape is
rejected. `path` scopes where the **symbol is looked up**, not where results
are listed.

## `show`

Returns a definition's decorated source — the item with its doc comments and
attributes — not just a location. Resolution, then: own the target's file,
check its range, take the item's extent from `documentSymbol`, widen it over
leading decorations with tree-sitter where a grammar exists, render one
header line and the body. Body budget 12,000 bytes; omission is noted, never
silent.

`view` selects something else about the same target:

| view | what it is |
| --- | --- |
| `definition` (default) | the decorated source |
| `outline` | the target's children, or a whole file's symbols with only `path` |
| `implementations` | `textDocument/implementation`, rendered as `uses` renders hits |
| `incoming-calls` / `outgoing-calls` | the call hierarchy, one line per caller/callee with its call sites under it |
| `type` | `textDocument/hover` reduced to its code blocks. With an anchor, rust-analyzer answers with the type of the expression the focus spans (`hoverRange`); other servers answer at its start. |
| `expanded` | rust-analyzer only: macro expansion at the focus |
| `related-tests` | rust-analyzer only: the tests related to the target, each with a runnable cargo command |

An unavailable view gets one of three distinct answers: the view name is
unknown (and the known ones are listed), no running server supports it, or it
does not apply to this target. Listings cap at 200 lines with the remainder
reported as a count.

## `uses`

References to the resolved identity: one source line per hit, with its
1-based line number and its enclosing item, grouped by file, tests set apart.

After resolution: own the identity's target file, check the name sits at the
target, wait, then `textDocument/references` there with
`includeDeclaration: false` — and drop any hit that still equals the target
range, for a server that ignores the flag. Every file with hits is then read
from disk and each hit's range checked against that text: it must hold the
name or one of the adapter's reference aliases (`Self`), or the server's view
of that file was stale and the call is an **error**, never a listing built on
it. Hits outside the root are counted, not listed.

References follow **symbol identity, not text**, so renames, re-exports and
method calls are found and same-named strangers are not. Cap: 100 hits, with
the remainder reported as a count; ordering is alphabetical by path and there
is no pagination or reference-side scope yet
(`../backlog/uses-scope-and-paging.md`).

## `action`

A named semantic change at a target, previewed before it is applied. Today
the one action is `rename` (with `new_name`).

Two identities are kept apart. The **intent** — target, action, arguments —
is re-resolved on every call. The **prepared edit** is what the server
returns for that intent *now*, shown as a preview of every changed line and
every file rename, with a `digest` over the files it touches.

- `mode = "preview"` (default) changes nothing and returns that digest.
- `mode = "apply"` requires the digest, **recomputes** the edit, and writes
  only if the digest still matches. Otherwise it returns a fresh preview and
  writes nothing.

After an apply, the servers are re-synced, so the next query sees the new
names. The edit is the server's: structural normalizes a `WorkspaceEdit` into
per-file text edits and file renames, locates them in the text it just read,
and applies them — it never composes edits textually, and it always declines
the server's own `workspace/applyEdit`. Writes are sequential, not
transactional; a forced shutdown mid-apply reports failure without claiming a
rollback ([shutdown.md](shutdown.md)).

A server whose rename has a known blind spot declares a caveat
(`rename_note` on the adapter, [servers.md](servers.md)); it rides as a
`note:` line on the preview and the applied summary alike, and on the fresh
preview a stale digest returns. The caveat is the adapter's standing policy
about the server's rename, phrased with "may", not a claim about any one
edit — a plan that does rewrite an intra-doc link carries it truthfully, and
a server without a declared blind spot adds no note.

## What every answer carries

Rendering lives apart from logic (`show/render.rs`, `uses/render.rs`,
`action/render.rs`, `listing.rs`, `hits.rs`) and obeys the byte budget in
[correctness.md](correctness.md): one line per entry, provenance in a footer
(which servers answered), and notes only when something is degraded — a cap
hit, a body truncated, a view unsupported, a server not running with its
install hint. The standing caveats — the fuzzy index is not proven
exhaustive, only exact names resolve, the root is fixed, a bare-name query
can miss a recent edit to a file no query has opened — are stated once in
the tool descriptions and in the MCP server instructions, not repeated per
answer.

Tool annotations: `show` and `uses` are read-only and idempotent; all three
are closed-world (they touch only the root they were started with).
