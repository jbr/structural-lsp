# The catalogue's measured set and order

Which language servers structural stands behind for each language it can
measure, in what order, and what was left out. Every row is a measurement
made through [`../tests/conformance/`](../process/testing.md): the questions
`definition`, `references` and `rename` need, asked of every candidate the
dataset records and this machine can start. Candidate argv comes from the
dataset (`../reference/servers-list/`), not from us. The ordering rule that
turns a score into an order is [`discovery.md`](discovery.md)'s; the
shipped entries are generated from this evidence by
`src/bin/catalogue.rs`, and a test in `src/discovery/catalogue/tests.rs`
fails when the shipped order and the stated one drift apart.

A row is one observation on one machine at one revision. The full per-probe
evidence, with server version, structural revision, fixture revision, and
date on every run, is checked in beside it —
`../tests/conformance/scores/<language>.json` — and a re-run overwrites the
same path, so a change to the evidence is a reviewable diff.

Consensus is the dataset's per-server source count, read from
`../reference/servers-list/by_executable.json`: every source recording the
executable, for any language, minus `mason-registry` — an installer index,
not a selector. Sources partly mirror each other (nvim-lspconfig and the
Zed extension registry among them), so the count is agreement, not
independence; it follows the binary name the dataset keys by, which for the
erlang server is `erlang_ls`.

## The rule, applied

Order is score answered descending, then consensus descending, then name
ascending — total, so a later run can be asserted against this document
rather than against a second run of itself. The score is what the probes
answered; it is not breadth of advertised capability, and the tiebreaks only
order servers that answered equally well. Consensus does not decide a
leader: typescript's measured leader below, `vtsls`, has the lowest
consensus of the three ts wrappers.

## Languages with more than one measured candidate

### Python

evidence `scores/python.json` · 10 candidates

| order | server | argv | answered | consensus | version |
| --- | --- | --- | --- | --- | --- |
| 1 | pyright | `pyright-langserver --stdio` | 8/8 | 9 | 1.1.403 |
| 2 | basedpyright | `basedpyright-langserver --stdio` | 8/8 | 6 | 1.39.5 |
| 3 | jedi | `jedi-language-server` | 7/8 | 6 | 0.47.0 |
| 4 | pyrefly | `pyrefly lsp` | 7/8 | 5 | 1.3.1 |
| 5 | zuban | `zuban server` | 7/8 | 4 | 0.10.0 |
| 6 | ty | `ty server` | 5/8 | 9 | 0.0.81 |
| 7 | pylsp | `pylsp` | 3/8 | 10 | 1.15.0 |
| 8 | pylyzer | `pylyzer --server` | 2/8 | 3 | 0.0.82 |
| 9 | ruff | `ruff server` | 0/8 | 8 | 0.16.10 |
| 10 | ruff-lsp | `ruff-lsp` | 0/8, errored | 3 | install broken |

### TypeScript

evidence `scores/typescript.json` · 5 candidates

| order | server | argv | answered | consensus | version |
| --- | --- | --- | --- | --- | --- |
| 1 | vtsls | `vtsls --stdio` | 8/8 | 5 | 0.3.0 |
| 2 | typescript-language-server | `typescript-language-server --stdio` | 7/8 | 9 | 4.3.3 |
| 3 | deno | `deno lsp` | 7/8 | 8 | 2.9.6 |
| 4 | biome | `biome lsp-proxy` | 0/8 | 6 | 2.1.1 |
| 5 | vscode-eslint-language-server | `vscode-eslint-language-server --stdio` | 0/8 | 4 | unread |

### Ruby

evidence `scores/ruby.json` · 5 candidates

| order | server | argv | answered | consensus | version |
| --- | --- | --- | --- | --- | --- |
| 1 | solargraph | `solargraph stdio` | 6/8 | 8 | 0.60.4 |
| 2 | ruby-lsp | `ruby-lsp` | 1/8 | 6 | 0.26.11 |
| 3 | rubocop | `rubocop --lsp` | 0/8 | 3 | 1.91.0 |
| 4 | steep | `steep langserver` | 0/8 | 3 | 2.1.0 |
| 5 | typeprof | `typeprof --lsp --stdio` | 0/8 | 3 | unread |

### C++

evidence `scores/cpp.json` · 2 candidates

| order | server | argv | answered | consensus | version |
| --- | --- | --- | --- | --- | --- |
| 1 | clangd | `clangd` | 6/8 | 11 | 17.0.0 |
| 2 | ccls | `ccls` | 5/8 | 4 | Homebrew build |

ccls's score understates it and that is a probe fact, not a server fact: its
references answer with the caller open and its index settled, but its index
progress is unawaitable by design (`progress_create_is_work = false`, the
adapter's workaround for a progress token it reserves and never reports
through), so the probe's wait finishes before its index does
(`../journal/2026-10-06-ccls-references-needs-the-caller-open.md`). The row
states the measurement; it is not adjusted.

### Erlang

evidence `scores/erlang.json` · 2 candidates

| order | server | argv | answered | consensus | version |
| --- | --- | --- | --- | --- | --- |
| 1 | elp | `elp server` | 3/8 | 5 | 1.1.0+build-2026-08-10 |
| 2 | erlang_ls | `erlang_ls --transport stdio` | 3/8 | 3 | 0.0.0 |

The two answered identical probe sets; the consensus count orders them.

## The one-candidate languages

The dataset records one candidate each. The order is trivially total, and
selection's reason is `OnlyCandidate`, not `HighestScore`
([`discovery.md`](discovery.md)).

| language | server | argv | answered | consensus | version |
| --- | --- | --- | --- | --- | --- |
| go | gopls | `gopls` | 8/8 | 10 | 0.23.0 |
| rust | rust-analyzer | `rust-analyzer` | 8/8 | 10 | 1.100.0-nightly |
| bash | bash-language-server | `bash-language-server start` | 4/6 | 11 | 5.6.0 |
| r | R | `R --slave -e languageserver::run()` | 4/8 | 6 | 4.6.1 |
| gleam | gleam | `gleam lsp` | 3/8 | 8 | 1.18.1 |
| qml | qmlls | `qmlls` | 3/8 | 4 | 6.11.1 |
| swift | sourcekit-lsp | `sourcekit-lsp` | 1/8 | 7 | unread |
| elm | elm-language-server | `elm-language-server` | 0/8, errored | 9 | 2.7.0 |

bash is asked fewer probes (six); the count is per run. elm-language-server
errors on every probe with an internal failure (`-32603`, `synchronize` /
`getTree` undefined) with and without the elm compiler and elm-format on
`PATH` — an errored row, not a scored zero, and not diagnosed further here.
`rust-analyzer` is the only candidate for `.rs` that has an argv at all: the
dataset also records `rls`, source-only, with no invocation.

## The cuts

Dataset candidates absent from the tables, and why — so the next person does
not relitigate them:

- **pyls** — unmaintained predecessor of pylsp, which is listed.
- **ruff-lsp** — deprecated upstream in favour of ruff's own server, which
  is listed; on this machine its install is broken by an lsprotocol
  mismatch, and its row records the errors rather than an answer.
- **flow** — the dataset records no argv for it; without one there is
  nothing to try, and inventing one is what the argv rule forbids.
- **rls** — the dataset records no argv (source-only in lsp-mode).
- **clice** — pre-release; not installed this pass.
- **srb** (sorbet) — not installed this pass; ruby's type-checker class is
  already measured twice (steep, typeprof).
- **rome, oxlint, eslint-language-server** — the diagnostics-only class.
  Its measured representatives are biome and vscode-eslint-language-server,
  both 0/8 with `textDocument/definition` unsupported, and ruff and rubocop
  likewise in python and ruby. lsp-mode's `add-on` marking was the
  hypothesis; the rows are the check.
- **markup and config trees** — toml, yaml, json, css, html, dockerfile,
  markdown, latex and their dataset candidates (taplo, tombi,
  yaml-language-server, …) have no probe: the harness asks symbol
  questions, and a tree without symbols needs its own shape (toml's
  keys-and-schemas is the named next case). Unmeasured entries rank zero
  when the catalogue carries them ([`discovery.md`](discovery.md)).
- **Next harness languages** — java (`jdtls`, only openjdk away), elixir,
  php, c#. Contribution targets, not cuts: the harness grows toward them.

## Arbitration, stated with what each choice beat

- **The score is the whole order; no admission threshold.** A bar would
  remove the only candidate for a language on exactly the two cases a zero
  can mean — a bug of ours, or a project never set up for the server
  ([`discovery.md`](discovery.md)). Ruff and rubocop stay in their tables at
  0/8 for this reason.
- **Consensus is the tiebreak, not the metric.** It stands in for
  maintained-and-preferred among servers that answered equally well, and is
  corrected for non-independence in the dataset's own terms (sources
  partly mirror each other). What it beat as a primary ranking: editor
  agreement itself — typescript's leader by measurement has the least of it.
- **Response time was considered and declined for the order.** A score is
  portable — whether a server answered is the same on any machine — while
  latency is a fact of the machine that measured it, and
  time-to-first-useful-answer is dominated by indexing, the hardest number
  to measure honestly. The harness can record timings if one later earns a
  promotion on evidence.
- **Modernity was declined.** The dataset records which editors ship a
  server, not release dates or activity; a modernity column would need a
  source the dataset does not have.
- **Editor labels enter only as a hypothesis to check.** No server is in or
  out of these tables on an editor's word: the diagnostics-only class is
  in the tables *because* lsp-mode marked it an add-on, and the measurement
  is how the marking was checked.
- **The root rule is not re-decided here.** It was settled by measurement
  ahead of this document; the rule and the comparison behind it are
  [`discovery.md`](discovery.md) and
  [`../reference/server-root-discovery.md`](../reference/server-root-discovery.md).

## Changing a row

```sh
cargo test --test conformance -- --ignored --nocapture <language>_servers
cargo run --bin catalogue
```

The first overwrites that language's score JSON; the second regenerates
`src/discovery/catalogue/generated.rs` from every score file and the
dataset, and the drift test in `src/discovery/catalogue/tests.rs` fails if
the shipped order and this document's stated order part ways. Installing a
further candidate is an entry in `tests/conformance/languages/<language>.rs`
with the argv the dataset records; the probe measures it on the next run. A
version that cannot be read is recorded blank, never guessed.
