# Slice 4: targeting, views, rename

Plan written 2026-09-14 after slice 3 (`docs/slice-3.md`). Three items, in
order, each its own commit and each usable on its own. The governing rule
of `docs/slice-2.md` is unchanged: no timer but the backstop, no retry, no
partial answer, no server string outside `src/lsp/adapter/`.

Why this order: rename needs an unambiguous target by definition, and bare
names alone cannot give one for `new`, `sync` or `execute` on any real
tree (`show new` on this repository lists a dozen impls with nowhere to go
from the listing). So targeting comes first; views reuse it and are cheap
once it exists; rename comes last and is the first mutation.

Deferred, with reasons: diagnostics (noise unless on request with rustc's
full `data.rendered`; separate experiment), SSR (a query-language question),
completion/inlay/code lens, test execution.

## 1. Targeting: qualified names and anchors

Today a target is `symbol` (bare) plus an optional `path` scope. Two
additions, both on `show` and `uses` (and `action` when it lands), sharing
`src/tools/resolution.rs`:

**Qualified names.** `symbol` may be `Config::new` or `Config.doubled`:
the last segment is the bare name sent to `workspace/symbol`; the earlier
segments must be a suffix of the candidate's `containerName` (split on
`::`, `.` or `/`, whichever the server uses). This is a *filter over the
same exact candidates*, not language-level path resolution: it narrows an
ambiguous listing, and the header of every result still shows where the
name went. A qualifier that eliminates every candidate answers "no
candidate of `new` is in a container ending in `Config`; candidates were
…", listing the containers the server reported, so a wrong guess is
visible and cheap to correct.

**Anchors.** `anchor` is quoted source text; `path` must then name a file.
The anchor must occur exactly once in the file as read from disk (zero:
"anchor not found"; several: "anchor occurs N times, at lines …; quote
more context"). `focus` is a substring of the anchor that must occur once
within it, default the whole anchor. The selection is the focus's range,
converted with the server's encoding, never computed by hand. Resolution
from an anchor is `textDocument/definition` at the focus start (the file is
synced first), then the same identity pipeline as a name: one target
resolves, several list, none is "no definition at `focus`". `uses` then
asks for references at the definition target as it does now, so a focus
on a local variable finds that local's uses and nothing else.

**Header for anchored targets.** The header's kind and name come from the
outline only when the outline symbol at the target *is* the target
(`Extent` grows a `selection_range`; equal ranges mean the same symbol).
A local variable is in no outline, so its header is the name at the
target range with no kind word. Never the enclosing function's name.

Done when: on the rust fixture `show new` with `symbol = "Config::new"`
resolves the constructor; on a scratch copy with a second `new`, the bare
name lists both and the qualified one resolves; `uses` with
`anchor = "let config = Config::new(3);"`, `focus = "config"` lists the
uses of that local and not the struct; pyright and gopls pass the same
shapes with their own container spellings; every error above is asserted
by text.

**Done 2026-09-14.** As planned, with three things learned:

- pyright's outline lists locals as `Variable` symbols with their own
  selection range, so an anchored local gets a confirmed header there
  (`variable config`); rust-analyzer's does not, so the same target is
  `config` with no kind word. Both are the rule working, not a difference
  to paper over.
- rust-analyzer reports no `containerName` for a free function even inside
  `mod tests`, so `tests::smoke` cannot be reached by qualifier; the
  container filter is for methods and class members, which is where the
  ambiguity is.
- The repo-backed identity test caught its second casualty
  (`anchored::resolve`); the resolver is `from_anchor`.

## 2. `show` views

`view` is an optional string on `show`; default `definition`. Names are
exact calling vocabulary, listed in the tool description; there is no
footer. An unsupported view is answered with one of three reasons, each a
one-line result: not applicable to this target, not supported by this
server, or (a view name nothing knows) unknown with the list.

Standard, every server:

- `outline`: with `path` a file and no `symbol`, the file's
  `documentSymbol` tree, one line per symbol: indentation for nesting,
  `kind name · line`, the outline `detail` when the server gives one,
  bodies omitted. With a target, the target's own children the same way
  (a struct's fields, an impl's methods, a module's items). Under the
  listing cap the rest is a count.
- `implementations`: `textDocument/implementation` at the target,
  rendered as `uses` renders hits (file, `line · item · source`), so an
  agent can tell the impls apart without opening them.
- `incoming-calls` / `outgoing-calls`: `callHierarchy/prepare` at the
  target, then the direction asked for, one line per caller/callee: `kind
  name · path:line`, with the call sites' lines under it (`uses` style).
- `type`: `textDocument/hover` at the target, the first code block of the
  contents (the signature or type), one line when it is one. For a focus
  that is an expression, rust-analyzer's `hoverRange` gives the type of
  the whole range; every other server answers at the focus start and the
  result says so when the focus was wider than a token.

Adapter-exposed, through `Adapter::extra_views()` (the empty hook from
slice 2), rust-analyzer only in this slice:

- `expanded`: `rust-analyzer/expandMacro` at the focus; the expansion text
  with the macro's name as the header. Not applicable when the focus is
  not a macro call.
- `related-tests`: `rust-analyzer/relatedTests` at the target, each test
  as its name, location, and the exact `cargo` command line (`cargoArgs`
  then `executableArgs`, quoted for a shell), which is what the survey in
  `docs/lsp-server-extensions-for-agents.md` rates highest for a harness
  with a shell.

Code: `src/tools/show/views/*.rs`, one file per view family, so `show.rs`
stays the dispatcher. Server methods (`implementation`, `call_hierarchy`,
`hover`) are generic on `LspServer`; the extension methods and their
params live in `adapter/rust_analyzer.rs`.

Done when: on the fixture, `outline` of `src/lib.rs` lists every `@decl`
item in order with methods nested under their impls; `implementations` of
`Label` lists the impl for `Config`; `incoming-calls` of `choose` names
`run`; `type` of the focus `config.doubled()` in an anchor is `usize`;
`expanded` of the focus `format!(...)` shows the expansion; a made-up view
name gets the unknown-view line with the list; `type` works on pyright
and gopls too.

## 3. `action`: rename

The third tool. `action(symbol|anchor+focus, path?, action, arguments?,
mode)` with `mode` one of `preview` (default) and `apply`. In this slice
`action` is `rename` with `arguments.new_name`; the listing of other
actions is item 4.

Flow, both modes:

1. Resolve the target exactly as `show` does; ambiguity is the listing, and
   nothing is renamed.
2. Sync the target file, wait, `textDocument/prepareRename` when the server
   advertises it (a "cannot rename here" is the answer); then
   `textDocument/rename` with the new name.
3. Normalise the `WorkspaceEdit`: `changes` and `documentChanges`, text
   edits per file, and `RenameFile` operations (rust-analyzer renames a
   module's file when the module is renamed). `CreateFile`/`DeleteFile`
   are refused as not supported in this slice, stated.
4. Every edited file is read from disk and every edit's range is checked to
   hold the old name (the same stale check as everywhere else). Edits are
   applied bottom-up per file in memory to produce the new text.
5. Preview renders: the resolved target's header, then per file the edit
   count and each edit as `line: <old line>` / `      <new line>`, file
   renames as `rename a → b`, and the digest: a hash over the ordered list
   of (path, on-disk text, edit json). Apply with no digest, or with a
   digest that no longer matches after recomputing, returns a fresh
   preview and changes nothing.
6. Apply writes each file, performs file renames, then syncs every owned
   file (a renamed file is closed under its old URI and opened under the
   new one) and waits for the server. The result is the preview's file
   list with `applied` and, for rust-analyzer, whether it went quiescent
   again.

`workspace/applyEdit` from the server stays declined; edits come back to
us as results, never pushed. Every edit is a `WorkspaceEdit` result in this
slice; a command-style action (typescript-language-server) is item 4's
problem.

Done when: on a scratch copy of the rust fixture, `rename choose →
pick` previews the definition and the call in `run`, `apply` with the
preview's digest changes both, `show pick` resolves and `show choose` is
not found; a second `apply` with the stale digest returns a fresh preview
without writing; renaming `Config` also rewrites `Self`-free sites and the
`pub use` re-export; a module rename on a scratch tree with `mod util;`
moves `util.rs`; pyright renames `choose` across `helper.py` and
`main.py`; gopls renames `Greet` in both files. All gated.

## 4. Other actions (if the slice has room)

`action` with no `action` given lists what the server offers at the target:
`textDocument/codeAction` at the target's range, one line per action as a
slug derived from its title (`extract-function`, `generate-impl`), with
`kind`. Applying one resolves it (`codeAction/resolve` when it carries
`data`) and goes through the same normalise/preview/apply path as rename.
An action that carries a `command` instead of an edit is reported as
"runs a server command; not applied in this slice". typescript-language-
server joins here as the third built-in, since its actions are the
command-style case; it is the item most likely to slip to slice 5.

## Testing conventions for this slice

- Anything that mutates runs on a scratch copy (the `both_servers` copy
  helper), never on `fixtures/` or this repository.
- New repo-backed assertions are not added; `identity.rs` stays the only
  one (see `docs/slice-3.md`, "For the next person").
- Pure parts (anchor location, edit normalisation and application, digest,
  outline rendering, hover extraction) get unit tests on fake data; every
  server path gets one gated test per server it claims to support.

## Verification

```sh
cargo test
cargo test -- --ignored
cargo clippy --all-targets -- -D warnings
cargo doc --no-deps
```
