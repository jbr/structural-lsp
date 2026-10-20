# Servers, adapters, and configuration

What each kind of language server needs said to it, and how a deployment
picks which ones to run. Implemented in `src/lsp/adapter.rs` (+
`src/lsp/adapter/`), `src/lsp/server.rs` and `src/config.rs`.

**No server-specific string belongs anywhere else.** Adding a server means
adding an adapter under `src/lsp/adapter/` or a `[servers.<name>]` entry in
`structural.toml` — never a special case in the transport, the tools, or the
workspace.

## `Adapter`: the seam

`Adapter` is everything the generic `LspServer` asks about one kind of
server. Every method has a default, so a new adapter is as small as a name,
a command and its languages:

| method | answers |
| --- | --- |
| `name`, `command`, `install_hint` | how to launch it, and what to tell a user when it is missing |
| `languages` | `Language { id, extensions }` — the routing rule's input |
| `initialization_options`, `experimental_capabilities`, `client_capabilities`, `position_encodings` | what to say at `initialize` |
| `readiness_policy` | an extra, server-specific readiness condition |
| `symbol_query` | rewriting a `workspace/symbol` query |
| `backstop` | this server's inactivity backstop |
| `extra_views`, `extra_view_request`, `render_extra_view` | non-standard views this server offers |
| `hover_params` | how `view = "type"` asks for a type |
| `reference_aliases` | names a reference may legitimately appear under (`Self`) |
| `kind_label` | the word shown for a `SymbolKind` |
| `rename_note` | a one-line caveat attached to this server's rename answers, when its rename has a known blind spot (rust-analyzer: intra-doc links) |

The seam is narrow on purpose. A behaviour seen on two or more servers, in
two or more languages, is a class: the fix changes what structural does with
*any* server's reply, in the generic path. A fix that names one server is a
case, and goes here or in the catalogue (`catalogue.md`) as data with
provenance — never in `src/` outside `lsp/adapter/`. The measurements the
classes were drawn from are `../reference/lsp-server-conventions.md`. A
server's own limit stays a `no`; what the generic path owes is that the
`no` is correct, explained, and not worse than the server's own behaviour.

## Readiness

The transport tracks *standard* readiness itself: the set of
`window/workDoneProgress/create` / `$/progress` tokens the server has opened
and not ended. "Standard idle" is that set being empty. A `ReadinessPolicy`
may add a stronger condition, and readiness is composed as

```text
ready = standard_idle && policy.is_ready()
```

so a policy can never override active standard progress. A policy that wants
a server-specific signal observes the notification stream through
`ReadinessPolicy::observe` and keeps its own protocol state — it never
rescans the event log, which is capped and diagnostics-only.

### A reservation is not always work

Counting `window/workDoneProgress/create` as an open token goes beyond the
protocol, where the create only *reserves* a token and work starts at
`$/progress` `begin`. It is counted because a server that creates a token is
about to start work, and the gap before `begin` is a window this connection
would otherwise call idle.

A server that reserves a token and never reports through it is therefore
never idle, and every readiness wait on it — including the one
`warm_index` does before the slot is published — blocks forever, which no
timer can shorten because the server answers liveness probes
(`correctness.md`). ccls is the measured case: one
`window/workDoneProgress/create` for an `index` token at startup, and no
`$/progress` frame of any kind afterwards, on a three-file tree and on a
two-hundred-file one alike. It indexes and answers queries the whole time.

`Adapter::progress_create_is_work`, `progress_create_is_work = false` in a
`structural.toml` entry, is how such a server says so. It narrows nothing
else: progress the server does report still opens and closes tokens
normally, and the default stays true for every server that begins the work
it reserved.

## The built-ins

**rust-analyzer.** `initialization_options` sets
`workspace.symbol.search = { kind: "all_symbols", excludeImports: true }`:
the default search looks at types first and falls back to everything only
when that is empty, so a function whose name fuzzy-matches some type would be
unreachable, and imports and re-exports reported as symbols would make every
re-exported name ambiguous. It advertises `serverStatusNotification`, which
the `Quiescence` policy observes, and `hoverRange`, which is what lets
`view = "type"` answer with the type of a whole expression rather than the
thing under one position. Its extra views are `expanded` (macro expansion at
the focus) and `related-tests` (which returns a runnable cargo command per
test). `expanded` is `rust-analyzer/expandMacro` at the focused token: a
macro call, an attribute macro and a proc-macro derive (at a token of its
path inside the parentheses, `Serialize` in `#[derive(Serialize)]`) answer
with the expansion. It answers `null` at `#`, `[`, `derive` and `(`, and at
any position for the builtin derives (`Clone`, `Copy`, `Debug`, `Default`,
`Eq`, `Hash`, `Ord`, `PartialEq`, `PartialOrd`). When the result is `null`
and the focus was quoted from source, the adapter reads the enclosing
`#[derive(…)]` from that source and the answer names which of those it is,
instead of "the focus is not a macro call"; elsewhere the generic line
stands. `render_extra_view` receives the quoted text and the focus's byte
offset for this (`ViewSite`); a name-addressed target has no quoted source
and gets the generic line. Its rename edit covers code references only — the reference search
does not visit doc comments — so `rename_note` attaches a one-line caveat to
every answer that carries an edit, telling the caller to check doc comments
for the old name rather than trusting the edit ([tools.md](tools.md)). An
answer with no edit to describe — the server cannot rename here, or returned
nothing — carries no caveat.

**pyright.** No extensions, no readiness signal beyond standard progress, and
the standard capability profile.

`builtins()` returns both; they are on by default.

## `structural.toml`

Read from the workspace root, which is the **project root** above the
directory a caller started in (`discovery.md`) — so configuration comes
from the project rather than from wherever an agent happened to begin.
Absent: the built-ins alone. Present but
unparsable: **nothing starts**, and every call reports the parse error —
silently ignoring it would run the wrong servers. Unknown keys are rejected.

```toml
idle_eviction_secs = 1800        # workspace-level: release a server no
                                 # query or bind has addressed for this long

[servers.rust-analyzer]          # a built-in's name: override it
command = ["/opt/ra/rust-analyzer"]
backstop_secs = 180

[servers.gopls]                  # any other name: a generic server
command = ["gopls", "serve"]     # required
languages = ["go"]               # required; extensions from the table below
extensions = { go = ["go"] }     # or spell them out

[servers.pyright]
disabled = true                  # not in this project, built-in or not
```

This file is the **override layer** over the server catalogue
(`discovery.md`), not the whole configuration. Naming a server configures
it, `disabled = true` takes it out of consideration, and a name the
catalogue does not know is added generically. What actually starts is what
selection plans from the tree's evidence, so a configured server whose
language is nowhere in the tree lays out no slot and starts nothing.

A disabled entry is never validated: `disabled = true` on a generic server
with no `command` is a statement about this project, not an incomplete
definition, and failing the whole file over it would leave the workspace
with no servers at all.

### The index is warmed at launch

Several servers answer `workspace/symbol` with nothing until some document
is open, and answer the identical query once one is — pyright,
basedpyright, pyrefly, clangd, typescript-language-server,
bash-language-server and R among nineteen measured
(`../reference/lsp-server-conventions.md`). Without a warm-up, a bare name
— the most common query shape — is answered `no symbol named 'x'` on a
fresh session about a symbol plainly in the tree.

So a launcher opens **one** file before the server is published:
`LspServer::warm_index`, gated on the advertised `workspaceSymbolProvider`,
picking the first file the adapter serves in a bounded, deterministic walk
(`src/lsp/server/warm.rs`). Any file will do — opening one makes symbols
defined in files never opened resolve — so the cost is one file read per
server, not one per file in the tree.

The open alone is not enough. pyright schedules the analysis an open
implies and fills its index from that, so a `didOpen` followed immediately
by a query still races it; the warm-up sends one `documentSymbol` against
the file it opened, which is what an editor sends on open anyway, and then
waits the same wait every query waits. There is no sleep and no retry: a
query that missed is never re-asked (`correctness.md`).

It happens on the launcher's own thread and before the workspace lock is
taken, so neither the walk nor the read can delay an unrelated query. A
workspace with no file of that server's language warms nothing, which is
not an error, and a server that does not advertise the capability is
skipped — pylsp, which has no index at all.

### Settings a server will not start without

`workspace/configuration` is answered with one entry per requested item,
and the entry is `settings` from the server's `structural.toml` table —
`null` when there is none, which hands the server its own defaults.

For some servers the default is to serve nothing. deno 2.9.6 answers every
request empty for the whole session unless the reply carries
`{"enable": true}`, and **ignores the same value in the initialization
options**: the configuration reply is the only channel it reads. Measured
both ways; with the setting it answers 6 of 8 probes and without it 0
(`../reference/lsp-server-conventions.md`).

```toml
[servers.deno]
command = ["deno", "lsp"]
languages = ["typescript"]
settings = { enable = true }
```

The same value answers every item whatever `section` it names. No server
measured here wants different settings per section, and a per-section table
would be a shape invented ahead of a case for it; `section` is in the
request when one turns up.

An entry named for a built-in **overrides** its command, languages or
backstop and keeps everything else — readiness policy, capabilities,
initialization options, query rewriting, install hint, extra views, hover
params, reference aliases, outline containers, settings. Any other entry is a generic
server, which needs `command` and `languages`.

`Configured` keeps that promise one hand-written method at a time: each one
forwards to `base` when there is one and answers the trait default when
there is not. Adding a method to `Adapter` that a built-in overrides means
adding a case to `Configured` **and** to the case in
`adapter/configured.rs` that asks an override every server-specific
question the built-in answers. Forgetting is silent and does not look like
a configuration problem — `outline_container` was missing, so an overridden
rust-analyzer compared `impl Config` to the `Config` at its range, called
the difference a disagreement, and failed a file-scoped `Config::doubled`
with a message blaming the outline for not describing the file.

Language ids with known extensions: `rust`, `python`, `typescript`,
`javascript`, `go`, `c`, `cpp`, `ruby`, `java`, `kotlin`, `zig`. Anything
else must supply `extensions`, and the error says so with the syntax.

Selection is not this: plans come from evidence, the catalogue and
overrides — one `ServerSlot` per plan, so a language the tree gives no
evidence for is never laid out, let alone started
([`discovery.md`](discovery.md)). The shipped catalogue is generated from
the measured evidence ([`catalogue.md`](catalogue.md)), and a configured
server keeps whatever measurement it carries.

## `LspServer`: one running server of any kind

`LspServer::spawn` produces a `Spawning` — a child that exists and can be
stopped but has not yet handshaken — and `Spawning::initialize` completes it.
The split is what lets shutdown reach a child mid-handshake
([shutdown.md](shutdown.md)). `LspServer::start` is the convenience of both,
for the paths that do not need the seam.

The server owns documents: `sync(path)` pushes the on-disk text with
`didOpen`/`didChange` and keeps the file owned for the rest of the
connection; `sync_owned` re-pushes every owned file before a query. Ownership
does not survive a fresh instance. `moved(from)` handles a file a rename
relocated.

Queries are thin wrappers that wait for readiness and then send:
`workspace_symbol`, `definition`, `references`, `implementation`,
`prepare_call_hierarchy` / `calls`, `hover`, `prepare_rename` / `rename`,
`document_symbol`, and a generic `request` for adapter-specific methods.
`supports(provider)` reads the negotiated capabilities, so a view that a
particular server cannot answer is reported as unsupported rather than
attempted.

`description()` is the one-line provenance (name and version) that answers
carry in their footers.
