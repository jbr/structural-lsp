# The tools

Three MCP tools, declared in `src/tools.rs`: `definition`, `references`, `rename` — LSP go-to-definition plus related views, find-all-references, and rename. The
same three run from argv (`structural-lsp definition choose`), since mcplease owns
both dispatch paths. All three take their target the same way and share the
resolution pipeline ([resolution.md](resolution.md)), so a target that
`definition` would list as ambiguous is listed identically by the other two.

## Targeting, common to all three

| argument | meaning |
| --- | --- |
| `symbol` | the name, bare (`choose`) or qualified by its container or module (`Config::new`, `Config.doubled`, `listing::render`). Exact match on the name; the qualifier ends a container or the file's path ([resolution.md](resolution.md#qualified-names)). |
| `path` | a file or directory under the root, relative to it. A file nominates `symbol` from that file's own outline rather than the workspace index, and picks its language server; a directory narrows the index. With `anchor`, the file the quoted text is in. |
| `anchor` | source text quoted verbatim from `path`, occurring exactly once in it — the way to reach a local, a call site, an expression. |
| `focus` | the token within `anchor` that is meant, occurring once in the anchor as a whole identifier; the whole anchor when absent. |
| `workspace` | another project to answer about for this call: a directory, absolute or relative to the caller's view. Its project root is discovered as at startup, so a subdirectory of a project answers from that project's servers about the subdirectory; `path` and the rendered paths are relative to it. Not a directory: an argument error naming `workspace`. |
| `language` | which language's servers the call starts and asks, by LSP id or extension, case-insensitively (`rust`, `python`, `ts`, `.py`). Without it a bare name asks every server here and same-named definitions are listed. One no server here speaks lists this workspace's languages; one a file `path` contradicts says which server serves the file. |

The default root is the caller's view, fixed at binding — for the MCP
front end the cwd, or `STRUCTURAL_ROOT`. `path` only scopes inside the
call's root: it is canonicalized, and a symlink escape is rejected. `path`
scopes where the **symbol is looked up**, not where results are listed.

`workspace` binds for the call and changes no default: the MCP session
binds the same agent through the same host, so the other root's servers
are shared with any other caller on it, warmed on the first call, and
released by the idle sweep; a cold start is reported by the progress
heartbeat like any first query ([ownership.md](ownership.md#a-call-in-another-workspace)).
`language` goes through the one routing rule that also decides what
starts, and its argument errors are decided on the workspace's planned
servers, never on which are running
([ownership.md](ownership.md#routing-a-scope-to-servers)).

## `definition`

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
| `outline` | the target's children; with only `path`, a file's symbols, or a directory's files with their top-level items (below) |
| `implementations` | `textDocument/implementation`, rendered as `references` renders hits — tests set apart — with results at one line folded into one row |
| `incoming-calls` / `outgoing-calls` | the call hierarchy, one line per caller/callee with its call sites under it, tests and code outside the root in their own sections (below) |
| `type` | `textDocument/hover` reduced to its code blocks. With an anchor, rust-analyzer answers with the type of the expression the focus spans (`hoverRange`); other servers answer at its start. |
| `expanded` | rust-analyzer only: macro expansion at the focus. A proc-macro derive expands with the focus on its name in `#[derive(…)]`; the builtin derives and the `derive` keyword have no expansion, and the answer says so ([servers.md](servers.md)). |
| `related-tests` | rust-analyzer only: the tests related to the target, each with a runnable cargo command |

**Several results at one line are one row.** A Rust derive is one
implementation per derived trait and one `textDocument/implementation`
result per trait, every one pointing at the `#[derive(…)]` line, with
nothing in the reply to say which is which. Rendered one per result that is
N identical rows carrying what one carries. They fold into the row that
shows the derive attribute — which names the traits — and a `note:` states
how many folded. The header keeps the server's count, because that is the
server's answer to "how many implementations", and nothing is inferred
about which result was which.

**Tests are set apart in every location listing.** `implementations`,
`incoming-calls` and `outgoing-calls` split their entries as `references`
does, through the same classifier (`hits::split_tests`, the heuristic in
[resolution.md](resolution.md)): production first, then a
`tests (heuristic) · N implementations` (or `callers`, `callees`) heading
and the test entries in the same shape. An implementation is classified by
its hit's file and enclosing chain; a caller or callee by its file and the
chain enclosing its name, from that file's `documentSymbol`. A server that
gives no outline for a file leaves its entries classified by path alone,
and a note says so.

An unavailable view gets one of three distinct answers: the view name is
unknown (and the known ones are listed), no running server supports it, or it
does not apply to this target. Listings cap at 200 lines with the remainder
reported as a count — except `outline`, which pages.

### The call views

In `src/tools/definition/views/calls.rs`, rendering in `calls/render.rs`.
An entry is `kind name · path:line`, its call sites under it as
`line · source`. Entries are in three sections: the workspace's, sorted by
path; its tests; and those whose file is outside the workspace root
(symlinks resolved first, as `references` resolves them), under
`outside the workspace · N callees`. The root is the only boundary used —
no server says which callees are a dependency's or the standard library's,
and a root narrower than the server's project puts the project's other
packages outside it too.

A callee outside the root is one line, `fn unwrap_or · called at 21, 24`,
in the order the target's body first calls each. Its call sites are lines
of the target, which is what was asked about, and its location is a path
into a toolchain or a dependency — measured on rust-analyzer, pyright and
gopls, a standard-library callee's absolute path ran 55 to 150 bytes, and
the same source line was repeated under every callee it calls (a chain
like `guard.first().copied().unwrap_or(0)` three times). Without the location two entries can print
the same (pyright reports each overload of `print` as its own callee); such
rows are one row ending `· 2 definitions`, and the header keeps the
server's count. A caller outside the root keeps the full shape: its call
sites are in its own file, and the path is how to reach them. The listing
caps at 200 lines across all three sections; the closing note counts the
entries it did not reach.

### `outline`

In `src/tools/definition/views/outline.rs` and the directory beside it. One
line per symbol, `kind name · line[ · detail]`, indented by nesting depth,
under a header that counts what is listed. Three shapes:

- **a target** (`symbol` or `anchor`): its children, from the outline node
  at the target, under the target's header line;
- **a file** (`path` alone): the file's whole tree;
- **a directory** (`path` alone, naming one): every file under it that a
  running server serves, in path order — `definition.rs` before
  `definition/render.rs`, a module file before its children's directory —
  each as a line of its path followed by its top-level items only. The walk
  is the evidence scan's: skipped and hidden directories are not descended,
  symlinked ones are not followed, and it stops at the scan's file budget.
  A file is routed by its extension alone — a file with no extension is no
  language's, rather than every server's as a file scope treats it. Page 1
  ends with what was left out: the files no server here serves, counted by
  extension; the files whose servers are not running, with the servers'
  states; and a walk that stopped at the budget. A file whose server has no
  `documentSymbol`, or whose outline is empty, is one line saying so.

**A function's locals are not outline material.** pyright lists a
function's parameters and local variables as `Variable` (or, for an
upper-case name, `Constant`) children of the `def`; vtsls does the same
inside a TypeScript function, method or constructor, and under a `Variable`
that binds an arrow function; rust-analyzer lists a `const` or `static`
declared in a function body as a `Constant` under the `Function`; gopls and
rust-analyzer list no `let` or `:=` bindings. Seen on two servers in two
languages, it is a class, so the rule is generic (`views/outline/locals.rs`)
rather than an adapter's: a `Variable` or `Constant` whose parent is a
`Function`, `Method`, `Constructor`, `Variable` or `Constant` is a local, and
it leaves the listing with its subtree. A nested function, class or struct
stays — it is an item. Module-level variables and class or instance
attributes stay — their parent is the file or the class. The header says how
many symbols left (`app.py · 11 symbols · 10 locals not shown`), and a
target's own direct children are exempt: `outline` of a function lists its
locals, which is where they are reached.

**Pages.** An outline is flattened to lines and cut into pages of 200,
counted from 1 by the `page` argument (refused on every other view). A page
that is not the last ends ``note: 100 more symbol(s); `page: 2` continues``;
a page after the first says `· page 2 of 2` in its header and starts with the
ancestors of its first line — the `impl` or class an earlier page opened —
so its indentation still reads as nesting; those context lines are not
counted against the page. A page past the end is an argument error naming
the last page. A directory outline outlines its files only as far as the
page asked for, so it cannot count its symbols ahead: its header counts
files, `page N of M` appears once every file has been read, and the
continuation counts the files not yet fully shown. Every page re-reads what
it lists from disk; a page is never served from an earlier call's outline,
so pages of a tree being edited between calls can disagree with each other,
and each is true of the disk when it was asked.

## `references`

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
method calls are found and same-named strangers are not.

Two arguments shape the listing without changing what is found. `within`
names a file or directory under the root and lists only the references
inside it; the header counts against the whole (`12 of 271 references · 4
files within src/agent/`), and a `within` that names nothing on disk is an
error about `within`. `path` stays the lookup scope, because it also picks
the server and the candidates — one argument with both meanings could not
say "find the `Config` in `lib.rs`, list its uses in `src/agent/`".

Files are listed in path order, production before tests, and a listing
stops at 100 references a page. A capped listing does not end in a bare
count: the header says which page is shown (`· page 1 of 3`), and the
remainder is summarized by directory, largest first, one line each
(`src/agent/ · 120 in 40 files`), at most twelve before the rest is folded
into one line. Each of those lines is a value for `within`, and the closing
note names the `page` that continues — counted from 1, as `outline`'s pages
are, and a page past the end is an error rather than an empty answer. Path order rather than a
relevance ranking keeps truncation explicable: nothing is dropped by a
judgment the caller cannot see, and the summary says where the rest is.
Every file's hits are still read and checked before the window is applied,
so the test split and the counts cover the whole listing, not the page.

## `rename`

A named semantic change at a target, previewed before it is applied. A
A rename at a target, previewed before it is applied.

Two identities are kept apart. The **intent** — target plus new name —
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

Rendering lives apart from logic (`definition/render.rs`, `references/render.rs`,
`rename/render.rs`, `listing.rs`, `hits.rs`) and obeys the byte budget in
[correctness.md](correctness.md): one line per entry, provenance in a footer
(which servers answered), and notes only when something is degraded — a cap
hit, a body truncated, a view unsupported, a server not running with its
install hint. The standing caveats — the fuzzy index is not proven
exhaustive, only exact names resolve, `path` scopes inside the root — are stated once
in the tool descriptions and in the MCP server instructions, not repeated
per answer.

Tool annotations: `definition` and `references` are read-only and idempotent; all three
are closed-world (they touch only the local workspaces they are pointed at).
