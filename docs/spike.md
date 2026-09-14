# Raw LSP spike

This is the throwaway first checkpooint from `design.md` ("First step", experiments
C and F): a binary, **no MCP surface**, that starts one or two language servers,
waits as long as it reasonably can for readiness, opens the annotated fixture
files, and dumps the raw results of `workspace/symbol`,
`textDocument/definition`, `textDocument/references`, and
`textDocument/codeAction` for every `@decl` and `@expr` annotation.

It is read-only by construction: it never applies an edit and never executes a
code-action command. `workspace/applyEdit` is answered `{"applied": false}`.

## Run

```sh
# both servers, default bounds
cargo run -- spike

# one server, custom bounds and output
cargo run -- spike --server rust --quiesce 120 --out target/spike
cargo run -- spike --server python --pyright-wait 6 --query-timeout 30 --out target/spike
```

`cargo test` covers the transport (framing, header caps, a scripted mock server)
and the annotation resolver; it does not need a language server.

Flags: `--server <rust|python|both>`, `--out <dir>`, `--quiesce <secs>`
(rust-analyzer quiescence bound), `--pyright-wait <secs>` (bounded wait for
pyright, which has no readiness signal), `--query-timeout <secs>`.

`STRUCTURAL_SPIKE_WIRE=1` prints a **diagnostic** trace to stderr: incoming
frames as shapes only (request/notification method, or response id), and
outgoing frames as JSON truncated to 240 characters. It is not a byte-exact
capture; the `raw/` files under `--out` are the unmodified server results.

## Fixtures

`fixtures/rust/` and `fixtures/python/` are annotated with two comment forms:

```text
// @decl <query> | <selection>      (Rust)
#  @decl <query> | <selection>      (Python)
// @expr <query> | <selection>
```

`query` is the symbol to resolve / the `workspace/symbol` query. `selection` is
the literal source text located after the annotation, used as the code-action
range. The resolver confines both lookups to the annotation's own region (up to
the next annotation), and records `target_provenance`: `query-in-selection` when
the query token was found inside the selection, `selection-start` when it was
not. It never searches beyond the selection for the query, so a missing symbol
cannot silently resolve to a later occurrence.

## Output

Deterministic per server under `--out` (default `target/spike/<server>/`):

- `raw/NNN-<kind>-<query>-<method>.json` — one file per query: the annotation,
  the exact params, latency, ok/error, and the **unmodified** result.
- `manifest.json` — initialize result and serverInfo, `initialize_elapsed_ms`
  (initialize only, not whole-run), readiness strategy and evidence, post-open
  `experimental/serverStatus`, `first_useful_result_ms`, code-action shape
  counts, a labelled within-process `warm_repeat`, every notification and
  server request seen, and query totals.

## Findings (rust-analyzer 1.100.0-nightly, 2026-09-01)

All numbers below are from the named capture in `target/spike/rust/`
(`manifest.json` plus `raw/`), the run whose summary printed 176 queries /
174 ok / 2 error / 90 non-empty. The fixture has 24 annotations: 12 `@decl` and
12 `@expr`.

- `initialize` 66 ms; `experimental/serverStatus` `quiescent=true` at **8.7 s**
  cold; post-open status still `health=ok, quiescent=true`.
- First useful (non-empty) result: **9.8 s** from process start. The
  within-process warm repeat (symbol + definition only) answered 42/48 in
  **14 ms**. Cold/warm is a large effect; experiment F is only partially
  evidenced because this is not a fresh-process warm run.
- 96 primary queries (24 each of `workspace/symbol`, `definition`,
  `references`, `codeAction`) plus 80 `codeAction/resolve` calls = 176 total,
  174 ok, 2 error, 90 non-empty. All 96 primary targets resolved
  `query-in-selection` (the resolve calls add files named after the action
  title).
- Latency is milliseconds except the largest `references` call at 1275 ms.
- **Every** returned code action is *unresolved*: 80 actions carried `data` and
  no `edit`; 78 of them produced an edit via `codeAction/resolve`. A client
  that does not advertise `codeAction.dataSupport` + `resolveSupport` sees an
  empty-looking menu. `workspace.workspaceEdit.resourceOperations` also matters:
  resolving "Extract module to file" fails `-32803 Client does not support
  create capability` because we do not advertise `create`.
- One r-a bug is preserved as evidence: resolving "Inline into all callers" on
  `decl label` panics the server (`TextDocument -offset overflowed`, in
  `ide_assists::handlers::inline_call::inline_into_callers`), reported as
  `-32603 request handler panicked`. The spike does not mask or retry it.

## Findings (pyright 1.1.403)

All numbers below are from the named capture in `target/spike/python/`. The
fixture has 20 annotations: 10 `@decl` and 10 `@expr`.

- No `serverInfo` is returned at all, so a known-server layer keyed on
  `serverInfo.name` cannot identify pyright; it would need the launch command or
  capability shape. No readiness signal either: pyright emits no `serverStatus`,
  so readiness is a bounded wait and the manifest says so instead of claiming
  completeness. `initialize` was 588 ms; first useful result 4.6 s.
- **Observed capability association (not a proven mechanism).** With the full
  client capability set, pyright answers `initialize` and `workspace/symbol` but
  never answers `textDocument/definition`, `references`, or `codeAction`
  (3 s–90 s bounds all expire). It sends `window/workDoneProgress/create`,
  receives our well-formed `{"jsonrpc":"2.0","id":N,"result":null}`, emits
  `$/progress`, then stops. A leave-one-out on the exact full set, two reps for
  the discriminating cases:
  - full, minus `general`, minus `experimental`, minus `window`, and
    `workspace` minus `symbol` or minus `configuration` all **fail** at 30 s.
  - **`workspace.workspaceFolders: true` removed** (so `configuration` + `symbol`
    remain) **answers in ~2.4 s**, as does removing the whole `workspace` block
    and the minimal `definition`-only set.
  What is established is an association: advertising
  `workspace.workspaceFolders: true` together with this rich `textDocument` set
  goes with pyright not answering document requests. It is not established *why*
  — a mechanism, an interaction with a particular `textDocument` sub-block, or
  "stops workspace loading" would all be overclaiming. (The pyright log line
  `Found 2 source files` is absent on the failing runs; that is evidence about
  pyright's internal state, not proof of what a capability did.) The result was
  reproduced with an independent Python client (blocking reader thread,
  `bufsize=0` + `os.read`, correct configuration replies), so it is not the Rust
  client, the framing, or mcplease.
- **The obvious concrete hypothesis was tested and disproved.**
  `workspace.workspaceFolders: true` promises the client will answer the
  server-initiated `workspace/workspaceFolders` request, and the transport did
  not. That handler is now implemented (it answers with the folders offered at
  `initialize`), but pyright **never sent `workspace/workspaceFolders`** in any
  trace (the independent probe answered it with `null`, not `MethodNotFound`, and
  still stalled), and advertising the capability with the handler in place still
  stalls. So the missing handler is not the cause.
- The spike therefore sends a verified profile that omits
  `workspace.workspaceFolders` (it still sends `workspaceFolders` in the
  initialize params). This is a workaround for a throwaway spike, documented
  here as an unresolved full-profile interoperability problem — not a claim
  that the servers agree in general.
- With that profile: 80 primary queries (20 each of `workspace/symbol`,
  `definition`, `references`, `codeAction`), 80 ok, 60 non-empty;
  `workspace/symbol` non-empty for all 20 annotated names. **`codeAction`
  returned an empty action list for all 20 document ranges** (10 declaration, 10
  expression) — pyright surfaced no refactors or quickfixes (no `command`-only,
  no edit-bearing, no `data`-deferred actions), unlike rust-analyzer. Latency is
  sub-millisecond at p50 (max 5 ms); warm repeat 40/40 in 21 ms.
- pyright sends `workspace/configuration` requests during the run, which the
  transport answers as one null per item; this is the only server in the two
  that exercises that handler.

## Reusable transport notes

`src/spike_transport.rs` is intentionally the *opposite* of the design's target
sans-IO shape: a synchronous client with a child process, a stdout reader
thread, and an `mpsc` channel. It reuses mcplease's JSON-RPC envelope types
as-is; mcplease has no `Content-Length` framer (its `stdio` is newline-delimited
MCP, its `headers` is HTTP) and its `ClientProtocol`/`Reaction` are
MCP-2026-07-28-specific, so framing and the server-request handler are
hand-written here. It bounds body size, header bytes, and header count; it
answers `workspace/configuration` (one null per item),
`window/workDoneProgress/create` and `client/register*` (null),
`workspace/workspaceFolders` (the folders offered at `initialize`), and
`workspace/applyEdit` (`applied:false`); unknown requests get `MethodNotFound`;
unexpected responses are preserved in `events`; cleanup kills and reaps the
child and bounds the reader join so an inherited stdout pipe cannot hang exit.
