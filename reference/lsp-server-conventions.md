# What language servers actually do, measured

Nineteen servers across thirteen languages, asked the same eight questions
through structural's tools on 2026-09-17. The harness is
`../tests/conformance/`; the per-run scorecards are what it prints, and
this is the reduction across them.

Every row is an observation on the version named, against a fixture whose
revision the scorecard records. Nothing here is a general claim about a
server — but where several servers differ in the *same dimension*, the
dimension is real even if the individual readings are not durable.

## The scores

| language | server | version | answered |
| --- | --- | --- | --- |
| rust | rust-analyzer | 1.100.0-nightly | 8/8 |
| python | pyright | 1.1.403 | 7/8 |
| python | basedpyright | 1.39.5 | 7/8 |
| python | jedi | 0.47.0 | 7/8 |
| python | ty | 0.0.81 | 7/8 |
| python | pyrefly | 1.3.1 | 6/8 |
| go | gopls | v0.23.0 | 6/8 |
| typescript | typescript-language-server | 4.3.3 | 6/8 |
| cpp | clangd | 17.0.0 (Apple) | 5/8 |
| python | pylsp | 1.15.0 | 3/8 |
| bash | bash-language-server | 5.6.0 | 3/6 |
| gleam | gleam | 1.18.1 | 2/8 |
| qml | qmlls | 6.11.1 | 2/8 |
| typescript | deno | 2.9.6 | 0/8 |
| swift | sourcekit-lsp | — | 0/8 |
| ruby | typeprof | 0.31.1 | 0/8 |
| elm | elm-language-server | 2.7.0 | 0/8 |
| erlang | erlang_ls | — | 0/8 |
| r | R languageserver | R 4.6.1 | 0/8 |

Bash is scored out of six: it has no types, methods or modules, so the two
qualified-name probes have no Bash spelling and are absent rather than
failed.

## The finding that justifies the method

**elm-language-server 2.7.0 advertises `workspaceSymbolProvider: true` in
its `initialize` reply, and answers `workspace/symbol` with `-32601
Unhandled method`.** Its other requests fail with `-32603` and an internal
JavaScript error (`Cannot read properties of undefined (reading
'getTree')`).

structural gates that request on the advertised capability and sent it
because the server said it could serve it. The gate is right and the claim
was false. This is the premise the probe was built on — a capability list
is a claim, and only asking settles it — arriving as a measurement rather
than an argument.

## The dimensions servers vary in

These are the axes, with who sits where. A client that assumes one answer
per axis is a client that works with the servers it was written against.

### 1. When the symbol index is populated

`workspace/symbol` is empty until something opens a file, on **six of the
nineteen**: pyright, basedpyright, pyrefly, clangd,
typescript-language-server (which answers with a TypeScript Server Error
rather than an empty list), and bash-language-server. Every one of them
answers the identical query once any file in the project is open.

rust-analyzer, gopls, jedi and ty index without being asked. pylsp has no
index at all.

This is the single most common failure in the sweep, and it hits the most
common query shape — a bare name with no `path`.

### 2. What `containerName` carries

`SymbolInformation` has one optional `containerName` and the specification
does not say what belongs in it.

| server | container of a method |
| --- | --- |
| pyright, basedpyright, pyrefly | the class |
| jedi | the **module** — and class members are not indexed at all |
| ty | `null`, for every symbol |
| gopls | the **package** import path |
| typescript-language-server | not the class |
| pylsp | no index |

### 3. Where the qualifier is written

Distinct from 2, and the one nobody expects: some servers put the container
**inside the name**.

| server | `workspace/symbol` name | `documentSymbol` name |
| --- | --- | --- |
| most | `doubled` | `doubled`, nested under the type |
| gopls | `Config.Doubled` | `(Config).Doubled`, **not nested** |
| rust-analyzer | `doubled` | nested under a node named `impl Config` whose range names `Config` |

Three spellings of one relationship, and `documentSymbol` nesting is the
only one of the three that is structural rather than a string.

### 4. Outline shape

`DocumentSymbol[]` (nested, with `selectionRange`) from most; flat
`SymbolInformation[]` from pylsp, which carries no range naming each symbol;
flat-but-decorated from gopls.

### 5. Symbol range convention

`location.range` is the enclosing range of the whole declaration for ty,
and the name alone for pyright. Both conform — `SymbolInformation` has no
`selectionRange` to carry the name separately, so the tight range is a
convention and not a rule.

### 6. Declaration and definition as separate sites

C++ is the only fixture where one entity is written in two files. clangd
resolves the bare name to the **declaration** in `helper.hpp`, and reports
no references from there, though `helper.cpp` defines it and `main.cpp`
calls it.

No other language in the sweep has the shape, so this is one observation on
one server — but every header language has it.

### 7. What an invocation buys

clangd without a compilation database indexes only what it opens; the
dataset records `clangd --background-index …` as an alternate invocation
with two sources behind it. The flags are not decoration, and "installed"
does not imply "invoked the way the capability needs".

### 8. The shapes of not answering

Seven of the nineteen answered nothing, and they did it in **five
distinguishable ways**. The distinction matters because structural's reply
to the caller is the same sentence for three of them.

| shape | servers | what structural says |
| --- | --- | --- |
| initializes, serves empty for every request | deno 2.9.6, sourcekit-lsp, typeprof 0.31.1 | `no symbol named 'choose'` — indistinguishable from a missing symbol |
| advertises a capability, then `-32601` on it | elm-language-server 2.7.0 | the error, surfaced |
| internal error per request (`-32603`) | elm-language-server | the error, surfaced |
| binary starts and exits | erlang_ls (`escript: exception error: undefined function erlang_ls:main/1`) | the error, surfaced |
| process closes stdout during `initialize` | R (`languageserver` not installed in R) | connection failure, with the install hint |

For deno, two hypotheses were tested directly and both failed:
`initializationOptions: {"enable": true}`, and a `deno.json` in the
workspace root. A third was not invented; the cause is unestablished.
sourcekit-lsp and typeprof were not investigated past the measurement —
sourcekit-lsp fails `anchor to a local` too, which needs no symbol name at
all, so its zero is not the argument-label naming difference that Swift
would otherwise predict.

The bottom two rows are servers that a `PATH` lookup reports as installed
and that a spawn reports as runnable. Only a handshake catches them.

The top row is the one that matters: **a server can be installed, runnable,
handshaken, and serve nothing**, and nothing downstream can tell that from
an empty workspace. Availability means answering.
