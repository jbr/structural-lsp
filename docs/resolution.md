# Resolution: from a target to one identity

Every tool shares one pipeline: a *target* — a name, or an anchor quoted from
a file — becomes one **identity**, or a listing that says why there is not
one. Implemented in `src/tools/resolution.rs` (+ `resolution/nominate.rs`,
`resolution/qualify.rs`, `resolution/anchored.rs`), over the pure helpers in `src/lsp/definition.rs`,
`src/lsp/anchor.rs`, `src/lsp/outline.rs` and `src/lsp/shape.rs`.

## Identity, not names

`workspace/symbol` only **nominates**. The pipeline:

1. **Sync** every owned document from disk.
2. **Wait** until each queried server has no outstanding work.
3. **Nominate** — the bare name becomes candidates from `workspace/symbol`
   (through the adapter's query rewriting), or, when `path` names a file,
   from that file's own outline (below). Candidates are partitioned into
   exact / fuzzy / ambiguous; only exact-name matches can resolve, and a
   name a server *decorated* counts as exact only when nothing matched the
   bare name ([Decorated names](#decorated-names)).
4. **Navigate** — every exact candidate's file is synced, its range checked
   against the text just read, and `textDocument/definition` asked at it.
   When the server also answers `textDocument/declaration`, that is asked
   too: a header language has two sites for one entity, and `definition`
   answers each from the other, so answering the hop lands on the
   declaration — a signature with no body — while the index named the
   definition. If the two requests agree with each other somewhere other
   than the candidate, the candidate is the definition and is answered
   directly; anything else keeps the hop. One hop, never a chase: the two
   sites answer with each other and following them does not converge.
5. **Group by target.** Candidates that navigate to the same place are one
   identity; candidates that navigate to different places are ambiguous and
   are listed with kinds, paths and signatures. Nothing is picked.

`parse_definition_result` normalizes defensively: `Location`,
`Location[]`, `LocationLink[]`, `LocationLink`, `null` and `[]` are all shapes
the spec allows, and all are handled.

Which servers are asked: a `path` naming a file goes to the server for that
file's language; otherwise every running server is asked and the candidates
are merged, with cross-server collisions tagged by the server that owns each.

## Nomination

`workspace/symbol` is a **bounded, fuzzy-ordered pool over the whole
workspace**, not a complete list: rust-analyzer truncates its answer (128
results on the version measured) after ordering the matches by its own
fuzzy score, so a common name's later definitions are simply not in it. That
bound is what a file scope must not depend on.

A `path` that names a **file** nominates from that file's own
`textDocument/documentSymbol` instead, in `src/tools/resolution/file_scope.rs`:

- the file is synced from disk first, so the outline is the server's view of
  the text just read, and the answer is current;
- a symbol's identity is the **name the server advertises**
  (`DocumentSymbol.name`) — or, when no node advertises the queried name,
  the identifier a node's advertised name decorates ([Decorated
  names](#decorated-names)) — and its own selection range is only the
  navigation point handed to the pipeline. That name is checked against the
  text at that range **at nomination**, before the candidate can reach a
  qualifier filter or a listing, and checked again by `definition_of` before
  navigating — so an outline whose range holds another identifier is an
  error on the listing paths too, never a line printed from a range the
  server no longer matches. rust-analyzer's `impl Config` node advertises
  `impl Config` and is therefore not a candidate at all — a query for the
  type finds the struct node;
- a method's **container** is each enclosing node's contribution, outermost
  first, joined by `::` (`nested::Deep`): the spelling the symbol index
  reports and what a qualifier filters on. What a node contributes is its
  own server's rule (`Adapter::outline_container`, `adapter/`): an ordinary
  symbol's range must name the symbol itself, and rust-analyzer's `impl`
  node contributes the type its header is written for — the *self type is
  parsed out of the advertised header* and compared with the range exactly,
  so `impl Label for Item` contributes `Item` and not the trait, and
  `impl<T> Wrapper<T>` contributes `Wrapper<T>` and not `T`. A shape no rule
  recognizes is an error, never the range's text taken to be a container — a
  wrong container selects a different definition. Rejection is conservative:
  a range shorter than a qualified self type (`impl crate::a::Item` with a
  range naming only `Item`) yields no container rather than a component of
  one;
- both outline shapes nominate. The hierarchical `DocumentSymbol[]` shape
  carries a `selectionRange` naming each symbol and says the container by
  nesting. The flat `SymbolInformation[]` shape carries neither: its
  `location.range` is the whole item, so the item's **declaration line** is
  searched for the advertised name — every language here writes a name on
  the first line of its declaration — and the container is read from the
  `containerName` field the server sets, which is the same claim the symbol
  index makes and is qualified on the same way. A declaration line that does
  not name the symbol, or names it twice, nominates nothing: the whole item
  is deliberately not searched, because a name recurs inside its own body
  and an ambiguous nomination is not one. pyright, ty and jedi serve the
  hierarchical shape; pylsp serves the flat one;
- a server without `documentSymbol`, a null outline, or an outline that
  names nothing exactly falls back to `workspace/symbol` narrowed to the
  file — so a file scope is never weaker than a bare name, and once it has
  candidates no index query happens at all.

A file scope's candidates are therefore the file's own symbols rather than a
sample of a bounded cross-workspace response — not a claim that the outline
is exhaustive: it is the server's statement about one file, and an item a
macro generated need not be listed. A qualifier that matches none of that
file's candidates is reported as an unqualified listing of them, never with
whatever the index would have offered. A **directory** scope still narrows
the bounded index, as a bare name does; both are brought current by the
query boundary ([correctness.md](correctness.md)).

Every scope is relative to the session's **view** — the working directory
the caller was bound with — and never to the workspace key, which may be a
project root above it (`discovery.md`). An agent in `crates/alpha` asking
for `path: "src"` means its own `src`, and a candidate in a sibling member
is outside its view and is counted as excluded rather than returned.

### Decorated names

A server's `name` is a display string, not a declared identifier. ccls
advertises `int choose(bool flag)` in its index, elp advertises `choose/1`
in an outline and erlang_ls advertises `choose/1` in the flat shape, all for
a definition the file spells `choose`. Exact name equality reaches none of
them, and the symbol reads as absent.

So when **nothing** matched the queried name, a second pass admits
candidates whose advertised name *decorates* it: mentions it exactly once as
a whole token (`definition::decorates`). The nominated identity is then the
**identifier**, not the decoration, and every check that follows is made
against it — the outline pass verifies it at the selection range before the
candidate can reach a filter or a listing, the flat shape searches the
declaration line for it rather than for the advertised name, and the
navigate step verifies it again in the text just read.

That ordering is the whole safety argument. A server that advertises the
bare name resolves exactly as it did; a decorated candidate never joins a
plain one; the range still has to hold the identifier in the file on disk;
and `chooser/1`, `choose_all` and `choose(choose)` are all refused — the
first two for want of a token boundary, the last because two occurrences do
not say which one is meant. Nothing here parses a language: no arity rule,
no signature grammar, no table of which server decorates how.

What it does not reach is an index that does not hold the symbol at all.
Both Erlang servers answer `workspace/symbol` for `choose` — and for
`choose/1` — with `[]`; elp's index holds modules, so `helper` resolves and
no function does. A bare name in Erlang is therefore still a miss, and a
`path` naming the file is the route that works.

### A language with no index

`workspaceSymbolProvider` is optional, and a server may serve documents and
no index at all: pylsp, gleam and qmlls each do
(`../reference/lsp-server-conventions.md`). Nothing is sent to such a
server — the capability is consulted before the request — so it contributes
no candidates, and a name defined in its language misses. Without a note,
that miss is spelled the same way as a name that is not in the tree.

A listing whose nomination could not ask every language therefore carries a
`note:` naming the languages with no index, and says that a name in one of
them resolves when `path` names its file — which is the path a file scope
takes anyway, from the file's own outline. The note names languages and not
servers: which executable serves a language is not the caller's problem, and
the
actionable part is the language and the `path` around it. It is omitted when
`path` already named a file, since that is the thing it would ask for.

The note is an availability statement, not a diagnosis: a bare name is sent
to every running server, so a workspace holding both an indexed and an
unindexed language carries the note on any miss, whichever language the name
was in. The alternative — refusing the query — is wrong whenever another
running server does have an index and answered.

A directory scope that finds nothing at all carries a second note when the
directory holds extensions no running server serves: the scope asked every
running server by design, so a tree in an unserved language would otherwise
miss as an absent name. The note names the directory, the extensions it
holds, and the unserved ones with the running servers — routing, not
absence. Served-ness is what the query observed (running servers), since a
configured server that is down serves nothing either. The extensions come
from the discovery budget walk, so build and hidden directories are
skipped; the note appears only on a total miss, never alongside fuzzy or
container-narrowable candidates.

A server that advertised an index, was asked, and has answered nothing all
session carries its own note: the server started but may not be configured
for this project. Answering means yielding once — a candidate from the
index, even one the filters remove, or a symbol from a file outline — so a
server that spoke once is never named again, and a server with no index to
ask is unindexed rather than silent. Like the directory note, it appears
only on a total miss, never alongside fuzzy or container-narrowable
candidates.

## Qualified names

`Config::new`, `Config.doubled`, `pkg/Greet`. The **last** segment is the
bare name sent to the index; the earlier segments are a *filter* over the
exact candidates by the container the server reported — the `containerName`
of an index candidate, or the enclosing symbols a file's own outline names
([Nomination](#nomination)) — not language-level path resolution. rust-analyzer and pyright report the impl or
class type as a method's container (`new` → `Config`) and report **nothing**
for a free function, so a module-path qualifier (`project::project_with`)
cannot resolve (`../backlog/module-path-qualifiers.md`). A qualifier matches
when the container's own segments end with it.

Because the qualifier filters the pool it was given, a common method name
whose pool is the bounded `workspace/symbol` response can fail to resolve
even though the symbol exists — in a bare-name or directory scope. A `path`
naming a file nominates from that file's own outline instead (see
[Nomination](#nomination)), so the qualifier narrows the file's own symbols.

A container has **more than one source**, and either confirming the
qualifier is enough — they are alternative places for a server to say the
same thing, not claims that have to agree:

- `containerName` on the index candidate, or the enclosing symbols a file's
  own outline names;
- the **name the server advertised**, when that name's last segment is the
  one being resolved. `SymbolInformation.name` is not specified to be a
  bare identifier and gopls puts the receiver in it — `Config.Doubled` from
  the index, `(Config).Doubled` from an outline — so the earlier segments
  are that server's spelling of the container. Only the ends of a segment
  are trimmed of punctuation, never the middle, and a name with no
  separator in it is never reinterpreted.

- the **file the symbol is in**, for languages whose module is a file:
  `helper.choose` in `src/helper.gleam`, `Config.doubled` in `Config.qml`.
  gleam and qmlls nominate a symbol and report no container from any other
  source, because there is no enclosing node to report. The stem is read
  from the candidate's own path, so it is a fact about where the symbol is
  rather than a guess about what it is called, and it is one source among
  several that never overrides another.

That second source also decides what counts as an **exact** candidate: a
name matches when its last segment is the name asked for. The identity that
results carries the name being resolved rather than the one advertised,
because that is what the file's own text holds at the range and what a
rename would replace.

The index is the weaker of the two pools for a qualifier, and by the
protocol rather than by implementation quality: `SymbolInformation` carries
**one optional `containerName`**, and what a server puts there is its own
choice. Of the six Python servers the conformance probe asks
(`../process/testing.md`), pyright and basedpyright name the class, jedi
names the *module* and indexes no class members at all, ty reports no
container for any symbol, and pylsp serves no index. `DocumentSymbol`
nests instead, so the container is the reply's shape rather than a field a
server may omit — and every one of those six resolves `Config.doubled`
once `path` names the declaring file. A qualified name that misses the
index therefore carries a `note:` saying so, because the caller cannot see
which of the two pools answered.

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

**Focus matches on identifier boundaries first.** The occurrences counted
are those where each edge of the focus that is an identifier character is
not continued by one in the anchor, so `boundaries` in `let (boundaries,
elided) = extract_boundaries(log);` is one occurrence and `new` is not found
inside `renew`. An identifier character is a Unicode alphanumeric or `_` —
the one rule for every language. `$` and `-` are not identifier characters:
they are in JavaScript, PHP, Lisp and CSS but sigils or operators in the
rest, and treating them as joiners would hide `foo` in Rust's `a-foo` from
the boundary pass. The cost is that in a dashed language `foo` in
`(foo-bar x)` matches as a boundary occurrence, as it did as a substring. A focus that begins or
ends in punctuation (`::new`, `$el`) is unconstrained on that side. Only
when no occurrence sits on a boundary does matching fall back to plain
substrings, which keeps a focus inside a string literal or a path
(`lib` in `"xlibx.rs"`) resolvable. Several occurrences in either pass are
refused with their count, and nothing in the answer says which pass found
the occurrence. A fallback hit inside a longer identifier is a position in
that identifier, and the server answers for it.

**The name is what the definition carries, not how the anchor spelled it.**
The two differ whenever the anchor is wider than the token it resolves —
`config.doubled()` resolves to `doubled`, and an anchor given with no
`focus` is the whole quoted text, so `choose(Mode::Fast);` resolves to
`choose`. That resolved name is what `verify_name_at` checks against the
target's range and what a rename would replace. Carrying the quoted text
there instead would make the check compare two different things and then
report the server's target as not naming it in the file on disk —
accusing the server of a mismatch this client constructed. An ambiguous
result is still rendered from the quoted text, which is what the caller
wrote.

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
