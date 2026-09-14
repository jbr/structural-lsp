# structural — design draft

Draft, 2026-09-13. Produced by a three-way brainstorm: jbr, a Claude Code session, and
an efference session (`calm-cliff`, gpt-6-astra) over the efference MCP bridge. Nothing
here is implemented. Open questions are at the end, each with the cheapest experiment
that would settle it.

## What this is

A standalone MCP server giving an agentic coding model LSP-backed code intelligence.
It assumes a neutral host: some harness that already has reasonable filesystem and
code-editing tools (read, search, edit). It is adjacent to `search` and to
semantic-edit-mcp, not a replacement for either. Rust first, rust-analyzer only; the
schema names nothing Rust-specific.

## Premise

A model's view of code is a token stream, not an editor viewport. It pays per byte,
cannot point with a cursor, and its memory of a file is a snapshot from N turns ago.

- The unit of code is the item (fn, struct, impl, mod), not the line. Lines exist only
  to line up with what the host's `read` shows.
- Results are pre-expanded to what the next click would have shown, because there is
  no next click.
- The first call has to beat grep, or the model keeps grepping. Every harness surveyed
  that ships an explicit LSP tool reports under-use (`lsp-in-agentic-harnesses.md` in
  the efference repo). A tool wins that contest by returning the answer (the item
  body, the candidate list with signatures) where grep returns a location.
- Capability is wide (standard LSP plus rust-analyzer's extensions plus ~150
  type-aware assists) but the schema stays small: three tools, language-neutral,
  with the rest of the capability reached through values that appear only in results.

## Architecture

LSP resolves, LSP queries, tree-sitter shapes.

1. **Resolution** goes to the language server (`workspace/symbol`, then
   `definition`/`hover` at the actual occurrence for anchored targets). tree-sitter
   nominates declarations; only the server establishes identity. `workspace/symbol` is
   a symbol-search interface, not language-level path resolution: candidates are
   filtered by file, container, and kind, and ambiguity is shown rather than resolved
   arbitrarily. The server's query syntax (r-a's `#`/`*`) stays internal.
2. **Queries** are standard LSP where possible, server extensions behind an adapter.
3. **Shaping** is tree-sitter: LSP `Location`s become decorated items (leading doc
   comments and attributes included, as in semantic-edit-mcp's `item.rs`) and
   enclosing-item groupings. The syntax layer is also the degraded path when the
   server is not ready.

## Tools

Three: `show`, `uses`, `action`. Host namespacing (`structural__show`) supplies the
prefix, so the names carry none. `action` is mutation only; every read expansion lives
on `show`. The most plausible fourth tool is a dedicated `apply`, added only if a host's
tool-level permission model cannot gate `mode="apply"` separately from
`mode="preview"`.

### Target

Structured fields, not a punctuation mini-language:

- `symbol`: a name or qualified path (`Config::from_toml`, `from_toml`). A partial or
  fuzzy match never silently becomes a resolved target, even when only one candidate
  comes back: the result says "no exact match; possible match", and mutation requires
  an unambiguous target.
- `path`: a file or directory scope. Optional beside `symbol` to narrow it; required
  for `anchor`.
- `anchor`: quoted source text that locates the context (a statement, a line).
- `focus`: the token or sub-expression within the anchor that is the selection. The
  distinction matters: definition needs a token, extraction needs an expression or
  statement range, and quoting a statement does not say which of `config`, `Config`,
  `from_toml`, `text` is meant.

1-based line:col is accepted as a last resort and is never the documented path. Every
result prints the resolved selection before any interpretation, so a targeting mistake
is visible before it becomes an edit.

### `show`

> Find a code definition by symbol name, or identify code using a file and quoted
> source anchor; returns the definition's code and documentation rather than just a
> location. Partial or ambiguous names return candidates with signatures and paths.
> Use `view="outline"` for an item inventory or `view="inspect"` for types, semantic
> relationships, and available actions.

`view` is an open string. Three documented values: `definition` (default), `outline`,
`inspect`. Any other value must be a view name a prior result advertised for that
target: `implementations`, `incoming-calls`, `outgoing-calls`, `related-tests`,
`type-hierarchy`, `expanded` (macro expansion), `signature`, and whatever else the
adapter exposes. Advertised names are exact calling vocabulary, not display titles.
Names with matching semantics stay stable across language adapters. Direct calls to a
supported view name work without prior advertisement. An unsupported view is answered
with one of three distinct reasons: not applicable to this target, not supported by
this server, temporarily unavailable.

Every result ends with a short footer of a few applicable read views and actions, not
only `inspect` results, so discovery happens by succeeding at the task the model already
wanted. The footer combines three sources: negotiated server capabilities, applicable
code actions at the resolved selection, and adapter-exposed extensions. It does not
compute reference counts (LSP has no cheap count independent of retrieval).

`inspect` sketches (illustrative; missing facts are omitted or marked unavailable, not
guessed):

```text
Config — struct · src/config.rs
pub struct Config
Documentation: Settings for a session.
Fields: model: String; timeout: Duration; tools: Vec<Tool>
Implementations: Config (src/config.rs); Default (src/config/default.rs)
Traits: Debug, Clone, Deserialize
Definition: 46 lines; show this target for its body.
Available reads: implementations · type-hierarchy
Actions: rename(new_name)
Assists: generate-constructor · generate-builder
```

```text
Config::from_toml — associated function · src/config.rs
pub fn from_toml(source: &str) -> Result<Self, ConfigError>
Documentation: Parse session configuration from TOML.
Defined on: Config
Definition: 38 lines; show this target for its body.
Available reads: uses · incoming-calls · outgoing-calls · related-tests
Actions: rename(new_name)
Assists at declaration: add-documentation-template
Body-specific assists: select an expression or statements with anchor.
```

```text
Expression · src/daemon/create.rs · inside create_session
Anchor: let config = Config::from_toml(&source)?;
Selection: Config::from_toml(&source)
Type: Result<Config, ConfigError>
Called function: Config::from_toml · src/config.rs
Signature: fn from_toml(source: &str) -> Result<Self, ConfigError>
Available reads: definition · type-definition
Assists: extract-variable(name) · extract-function(name)
Selection excludes `?`; selecting the enclosing expression changes its type.
```

Inspecting a function has no single natural code-action range: at its name, over its
body, and at one expression produce different menus. The result states which selection
was inspected.

`outline` rules: source order preserved; methods grouped under their impl; signatures,
not names alone, with long bounds and attribute lists collapsed and marked abbreviated;
child modules listed but not recursed; re-exports shown as re-exports; imports and doc
prose omitted; under budget pressure, field detail goes before function signatures. A
file outline describes that file's syntax; a module outline may include out-of-file
children and semantic exports, and the result says which it is. A workspace outline
(`path="."`) lists packages, source roots, and top-level modules, not every symbol.

```text
src/config.rs — outline
18 declarations (nested included); bodies omitted; abbreviated where marked.

struct Config { model: String, timeout: Duration, … 6 more fields }
enum ConfigError { Parse, InvalidModel, … 2 more variants }
impl Config
  pub fn from_toml(source: &str) -> Result<Self, ConfigError>
  pub fn validate(&self) -> Result<(), ConfigError>
impl Default for Config
  fn default() -> Self
fn normalize_model(name: &str) -> String
mod test [test] — src/config/test.rs

Shown: 9 of 18 declarations. Continue: <cursor>
```

### `uses`

> Find references to a resolved symbol, grouped by enclosing item with signatures and
> source excerpts, separating tests where identifiable. Unlike text search, this
> follows symbol identity; output reports coverage and omissions, and says when
> semantic results are unavailable. Accepts a symbol name or a file and quoted source
> anchor.

Budget order: resolved identity and completeness, then the enclosing-item inventory,
then representative excerpts, then full small items only when they fit. Counts of
references, items, and files are kept separate. References outside any named item are
preserved. Test classification is labelled heuristic when it is. If indexing is
incomplete the result says "N returned; indexing incomplete", never "N total".

```text
Config::from_toml — 80 references
Declaration excluded. 24 enclosing items in 11 files.
Production: 52 references / 15 items · Tests: 28 references / 9 items

PRODUCTION
src/daemon/create.rs
  create_session(...) -> Result<Session>                  [6 refs]
    let config = Config::from_toml(&source)?;
    … 5 more occurrences in this item
src/config/load.rs
  load_project_config(path: &Path) -> Result<Config>      [3 refs]
    Config::from_toml(&std::fs::read_to_string(path)?)
… 13 further production items listed by signature …

TESTS
src/config/test.rs
  defaults_are_applied()                                  [4 refs]
… 7 further test items …

Shown: all 24 item summaries; excerpts from 5 items. Next excerpt page: <cursor>
```

### `action`

> Preview or apply a named semantic action at a target, such as rename, or an action
> returned by `show(view="inspect")`. Action listings describe required arguments;
> edits are previewed before application, and application rejects changes to the
> source or proposed edit since that preview. Actions that are unavailable or
> ambiguous return alternatives rather than guessing.

Two identities, deliberately separate:

- **Intent**: `{target, action: "fill-match-arms", arguments}`. Readable, re-resolvable
  at call time, nameable in prose across turns, auditable by a permission gate. Stable
  names (`rename`) cover common intents; discovered assists are named by a slug the
  adapter derives, with provider identifiers used where the server supplies them. If a
  title-derived name is ambiguous at a target, the result returns the distinguishing
  selection or context, not an ordinal.
- **Prepared edit**: the exact patch and its source preconditions. `mode="preview"`
  returns the patch and a digest of (affected source, resulting edit). `mode="apply"`
  re-resolves, recomputes, and applies only if the digest matches; otherwise it returns
  a fresh preview. No server-side handle table. The digest is a confirmation field, not
  the action's name. A preview is not a permission gate; the host's gate is.

Every action result states whether it reads, edits, or executes. Actions that carry a
server command rather than a complete `WorkspaceEdit` are re-run, not replayed. Nothing
in `action` executes workspace code: "related tests" is a `show` view that returns
runnable commands for the host's shell.

`action` requires a target in v1.

## Readiness

rust-analyzer cold-starts in seconds to tens of seconds and answers little until
indexed. `experimental/serverStatus` (health, quiescent) is the readiness signal;
treat it as evidence, not a completeness certificate.

The server process starts when this MCP server starts, which is harness boot, so
indexing usually finishes before the model first reaches for a tool (jbr, 2026-09-13).
Given that, the first experiment does not build a syntax-only workspace fallback:

- A call that arrives before quiescence waits, bounded (order of tens of seconds),
  then answers with status rather than a guess.
- `show` on a single file can still answer from tree-sitter alone during the wait
  (per-file parse is cheap and semantic-edit-mcp already does it), labelled
  `semantic server indexing (12s); syntax-only result`. No workspace-wide declaration
  index on the degraded path.
- `uses` never substitutes textual matches:

```text
Semantic references unavailable: server indexing (12s).
Target found syntactically: Config::from_toml, src/config.rs.
No reference query completed; this does not mean there are no uses.
Retry when indexing finishes. For textual occurrences, use search.
```

Open question F below asks whether even this much is needed once the server is warm
from boot.

## Out of scope for the first experiment

- Diagnostics. Not because the host's `cargo check` covers it (the survey's attribution
  argument, baseline-diff and cross-file propagation, stands) but because it is a
  separate experiment from navigation and contextual actions.
- Structural search and replace (r-a `experimental/ssr`). It tests whether models can
  author the server's pattern language, a different question. When it lands it takes an
  explicit workspace scope; "no target" never means "everywhere".
- Completion as API enumeration, signature help, speculative edits, inlay-annotated
  reads, code lens, semantic tokens.
- Test execution of any kind.

## rust-analyzer capability inventory (adapter-internal)

Standard: definition, declaration, typeDefinition, implementation, references, hover,
documentSymbol, workspace/symbol, call hierarchy, type hierarchy, rename/prepareRename,
codeAction (the ~150 assists arrive here), completion, signatureHelp, pull diagnostics.

Extensions worth reaching, in rough order: `experimental/serverStatus`,
`rust-analyzer/expandMacro`, `experimental/ssr`, `rust-analyzer/relatedTests` and
`experimental/runnables`, `rust-analyzer/getFailedObligations`, `hoverRange` (verify),
`rust-analyzer/runFlycheck` (only with diagnostics), `reloadWorkspace`,
`rebuildProcMacros`. Skipped: view* debugging requests, joinLines, onEnter,
matchingBrace, moveItem, test-explorer notifications.

Extensions are opt-in per server through `initialize`'s `experimental` client
capabilities plus custom method names; that is the whole adapter contract per
extension.

## Implementation choices (sketch)

- **Transport is a stdio JSON-RPC client, no framework.** LSP and MCP are both
  JSON-RPC 2.0 over stdio; LSP frames each message with a `Content-Length:` header
  block instead of a newline, and the server sends requests the client must answer
  (`workspace/configuration`, `window/workDoneProgress/create`,
  `client/registerCapability`, `workspace/applyEdit`) plus a steady stream of
  notifications (`publishDiagnostics`, `$/progress`). The shape is the one efference
  already uses for MCP providers: a sans-IO protocol state machine (mcplease's
  `ClientProtocol`, `Reaction`, `Negotiation`) beside a framing-only pipe
  (`src/mcp/stdio.rs`). The LSP version needs a header framer, an id-to-response
  map because LSP calls overlap, and a handler for server-initiated requests.
  Reuse mcplease's `JsonRpcMessage` types as-is. No tower.
- **Types:** `lsp-types` 0.97. rust-analyzer's extensions are not in it; each is a
  small hand-written params/result pair.
- **Syntax:** `tree-sitter` 0.27 and the grammars semantic-edit-mcp pins; the item
  layer (`item.rs`) is extracted into a shared crate when both need it.
- **Two servers from the first slice**, so the generic path is never shaped by
  rust-analyzer alone: rust-analyzer and pyright (fast start, heuristic references,
  class/decorator container model, no extensions). typescript-language-server joins
  when `action` lands: its code actions arrive as commands, not edits.
- **Adapter:** one generic adapter driven by the capabilities declared at
  `initialize`; a known-server layer keyed on `serverInfo.name` registers extra views
  and actions. Any server the user configures (command plus languages) gets the
  generic path; the known ones get more.
- **First step:** a throwaway spike binary, no MCP surface, that starts both servers,
  waits for quiescence, and dumps raw `workspace/symbol`, `definition`, `references`,
  and `codeAction` results for a dozen declarations and a dozen expressions each.
  That is experiments C and F below, and it decides the transport shape.

## Open questions, with the cheapest experiment for each

A. **Three tools may still lose to search.** Mount them beside ordinary filesystem
tools with no usage instruction; run name lookup, ambiguous-name, dependency-definition,
and caller-discovery tasks; record first tool choice, calls-to-useful-code, output
bytes, wrong-target selections; compare against filesystem tools alone.

B. **A default definition body may be too expensive.** For large structs, impls,
modules, compare body-first against size-triggered outline-first; measure whether the
next call fetches the body anyway and whether either misses members. Set the cutoff from
traces, not an aesthetic line limit.

C. **Inspect menus may advertise less than the catalogue suggests**, because many
assists need precise expression or statement selections. Inspect a dozen real
declarations and a dozen selected expressions against r-a directly and save the
returned actions and their required selections. Testable before any MCP façade exists.

D. **Anchor plus focus may still be underspecified.** Give the resolver twenty natural
quoted targets from real code (repeated calls, chained expressions, multiline
selections), have a model choose targets without positions, count ambiguity, wrong
spans, and extra calls. Add no richer grammar until the failures name what is missing.

E. **Dynamic views may add an unnecessary round trip.** Allow direct calls to supported
view names from the start; observe guessed names and recovery from unsupported ones; if
synonyms recur, consider aliases before expanding the schema.

F. **Readiness may dominate first use.** Same navigation tasks, cold and warm r-a, small
and substantial workspace; measure time-to-useful-result and whether the model abandons
the semantic tools after one unavailable response. Decides whether a persistent warm
server is a prerequisite rather than later infrastructure.

G. **One mutation tool may be the wrong permission boundary.** Mount in the intended
hosts and exercise preview, approval, apply, including a source change between preview
and apply. If preview needs repeated manual gates or apply cannot be isolated, split
`apply` out.

Also unverified: r-a's exact `workspace/symbol` query semantics, and whether
`hoverRange` still exists under that name.
