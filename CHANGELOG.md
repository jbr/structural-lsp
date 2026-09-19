# Changelog

All notable changes to structural. Format follows
[keep-a-changelog](https://keepachangelog.com); one line per feature. The
project is unreleased, so everything lives under *Unreleased* for now. For
how the system works, see [`docs/`](docs/); for the narrative behind a
change, see [`journal/`](journal/); for what is left, see
[`backlog/`](backlog/).

## [Unreleased]

### Fixed
- **An anchor with no `focus`** (docs/resolution.md): the name is what the definition carries rather than how the anchor spelled it, so `choose(Mode::Fast);` resolves to `choose` instead of erroring that the server's target "does not name it in the file on disk".

### Added
- **Slots come from plans** (docs/discovery.md, docs/ownership.md): a workspace lays out a slot per planned server rather than per configured adapter, so a Python-only tree starts no Rust server; `structural.toml` becomes the override layer over the catalogue and gains `disabled = true`, which it could not express before.

- **Discovery, as pure policy** (docs/discovery.md): evidence + catalogue + overrides → server plans with reasons, a value no process is started from — ranked by what a server answered through `show`, `uses` and `action` rather than by how many editors ship it, with no admission threshold and no score statable without the run that produced it.

- **No-index note** (docs/resolution.md): a name that missed says which languages have no symbol index to ask — pylsp, gleam and qmlls serve documents and no index — and that naming their file in `path` resolves from its outline, instead of reporting the symbol absent.

- **TypeScript and C++ conformance columns** (process/testing.md): eleven servers across five languages measured, reduced to the dimensions they vary in (reference/lsp-server-conventions.md) and four aggregate units (backlog/generic-baseline/).

- **Rust and Go conformance columns** (process/testing.md): the probe is per-language — rust-analyzer answers 8/8, gopls 6/8, and the Go failures are `backlog/server-spelled-qualified-names.md`.

- **Fixtures as a specification** (src/fixtures.rs): the trees are checked in as `.in` text and materialized into scratch directories rather than living in the repository, so structural pointed at itself sees one language; `cargo run --bin fixtures -- <dir>` writes them out to work on by hand.

- **Qualified-name scope note** (docs/resolution.md): a qualified name that missed the symbol index says that naming the declaring file in `path` resolves the qualifier from that file's outline — the index carries at most one `containerName` per symbol and several servers report none, where `documentSymbol` nests.

- **Paired probe columns** (process/testing.md): `qualified name, in file` alongside `qualified name`, so a failing cell localizes to the index or the outline rather than being ambiguous between the server and this code.

- **Conformance probe** (process/testing.md): `tests/conformance/` asks every installed server of a language the questions `show`, `uses` and `action` need, and prints a scorecard with each server's version — what it answered, not what it advertises.

- **Bare-name staleness regression** (process/testing.md): feature-gated pending acceptance — an unopened-file edit and a newly-created file, each followed immediately by a bare-name query — observed failing on rust-analyzer, with the currentness wording qualified to match the implementation (docs/correctness.md).
- **`action` with `rename`** (docs/tools.md): a semantic change previewed with a digest over the files it touches, applied only when the digest still matches, with the servers re-synced afterwards.
- **Rename caveat note** (docs/tools.md): an adapter-declared `rename_note` rides as a `note:` line on every rename preview and applied summary — rust-analyzer's rename edit covers code only, so the caller is told to check doc comments for the old name.
- **`show` views** (docs/tools.md): `outline`, `implementations`, `incoming-calls`, `outgoing-calls`, `type` (via rust-analyzer's `hoverRange`, so an anchor yields the type of the expression), plus rust-analyzer's `expanded` and `related-tests`, which returns a runnable cargo command per test.
- **File-scoped nomination** (docs/resolution.md): a `path` naming a file nominates from that file's own `documentSymbol` instead of filtering the servers' bounded `workspace/symbol` response, so a common qualified name resolves from the file that declares it — the candidate is the name the server advertises and the file's own text is what verifies it; a directory scope still narrows the index.
- **Qualified names and anchors** (docs/resolution.md): `Config::new` filters candidates by the container the server reports; an anchor quoted from a file — with `focus` naming the token — skips the symbol index and reaches locals, parameters and call sites.
- **`uses`** (docs/tools.md): references to a resolved identity, one source line per hit with its enclosing item, grouped by file, tests set apart by a heuristic that says it is one, with every hit's range verified against the text just read.
- **Externally escalatable shutdown** (docs/shutdown.md): `request_shutdown` closes admission without waiting, `force_shutdown` interrupts starting and running servers without taking a workspace lock, `wait_shutdown` waits for admitted work and cleanup. Unix signals escalate — first soft, subsequent force — and real input EOF forces cleanup in the reader.
- **Owned process groups** (docs/shutdown.md): on Unix each server leads its own process group, and cleanup signals the group before reaping its leader, so descendants holding inherited pipes do not survive teardown.
- **Explicit server lifecycle** (docs/ownership.md): `Cold | Starting | Running | Failed` per slot, with concurrent demand electing exactly one launcher, launches running on their own threads holding no lock while they spawn and initialize, publication bounded so a busy workspace cannot strand a child, and every exit path — including a panic — waking the waiters.
- **The shared host** (docs/ownership.md): workspaces keyed by canonical root and shared by every caller bound to one, with per-agent interest, per-workspace serialization, and independent progress across unrelated roots. One MCP process and an embedded consumer's many agents enter through the same door.
- **The adapter seam, pyright, and `structural.toml`** (docs/servers.md): per-server knowledge behind an `Adapter` trait — command, languages, capabilities, readiness policy, query rewriting, extra views — with built-ins overridable and any other command configurable as a generic server.
- **The threaded LSP handle** (docs/transport.md): one server on reader/owner/writer threads with every queue bounded, one inactivity backstop as the only timer, and a `$/structural/ping` liveness probe so a quiet-but-busy server is not mistaken for a broken one.
- **`show`** (docs/tools.md): a symbol resolved to its definition's decorated source — doc comments and attributes included — with the extent from the server's `documentSymbol` and tree-sitter widening it over leading decorations.


### Changed
- **Resolution is by identity, not by name** (docs/resolution.md): every exact `workspace/symbol` candidate is sent to `definition` and the results grouped by target, so candidates that navigate to the same place are one answer and candidates that differ are an ambiguous listing with kinds, paths and signatures. Nothing is silently picked.
- **Answers are trimmed to what an agent needs** (docs/correctness.md): one header line plus the body on the happy path, one line per listing entry, `note:` lines only when something is degraded, and the standing caveats stated once in the tool descriptions rather than in every result.
- **rust-analyzer's symbol search is configured `all_symbols` with `excludeImports`** (docs/servers.md): the default searches types first and reports re-exports as symbols, which made some functions unreachable and every re-exported name ambiguous.
- **Every source file stays under about 500 lines** (CLAUDE.md), with tests in a sibling module.

### Fixed
- **A qualifier resolves from any source that carries it** (docs/resolution.md): `containerName`, an outline's nesting, the symbol's own name, or the file when a module is a file — any one confirming the qualifier is enough. gopls spells a method `Config.Doubled` and answers 8 of 8 probes where it answered 6; gleam and qmlls resolve a file-scoped qualified name they could not.

- **`implementations` renders a derive as one row** (docs/tools.md): a derived trait is one implementation result per trait, all at the `#[derive(…)]` line with nothing to tell them apart, so the identical rows fold into the one that names the traits and a `note:` says how many folded; the header keeps the server's count.

- **A cold symbol index no longer reads as a missing symbol** (docs/servers.md): a launcher opens one file the server serves and asks for its outline before publishing it, so a bare name resolves on a fresh session — pyright and basedpyright answer 8 of 8 probes where they answered 7, and five more servers gained their cold bare-name column.

- **A server that needs client settings can be given them** (docs/servers.md): `settings` in a `structural.toml` entry answers the server's `workspace/configuration` request, which some servers read and nothing else substitutes for — deno 2.9.6 served nothing at all for a whole session without it, and answers 6 of 8 probes with it.

- **An outline container's range may be wider than the name** (docs/correctness.md): the advertised name is located inside the selected text at a token boundary instead of compared for equality, so a server that selects a whole assignment reports a container; zero or several occurrences still refuse, and the refusal no longer names a cause it has not established.

- **A configured override of a built-in keeps all of its knowledge** (docs/servers.md): `Configured` did not forward `outline_container`, `extra_views`, `extra_view_request`, `render_extra_view`, `hover_params` or `reference_aliases`, so naming rust-analyzer in `structural.toml` to point at a specific binary silently lost impl containers, the `expanded`/`related-tests`/`type` views and `Self` aliases.

- **A symbol range wider than the name resolves** (docs/correctness.md): `SymbolInformation.location.range` is the range enclosing the symbol, so a whole-declaration range is conforming; the name is located within it and the follow-up request is sent at the name. A range holding no occurrence is still refused, and two are ambiguous. The refusal no longer claims staleness it has not established.
- **`workspace/symbol` is not sent to a server that did not offer it** (docs/servers.md): a `-32601` used to end the whole query, including file-scoped ones that never needed the index.
- **A refused outline degrades `uses` instead of failing it** (docs/tools.md): the enclosing-item column is dropped with a note saying why, distinguishing a server without `documentSymbol` from one that would not outline an unopened document.
- **The flat `SymbolInformation[]` outline nominates** (docs/resolution.md): its declaration line names the symbol and its `containerName` field names the container, so a file scope resolves against servers that serve that shape.
- **A blocked tool call no longer blocks MCP `ping`** (journal/2026-09-14-generic-kernel-and-tools.md): mcplease's stdio server answers `ping`, `initialize` and `tools/list` on its reader thread while tool calls queue, so waiting as long as a server needs does not make the mount unresponsive.
- **A failed connection keeps answering with its reason**: the owner thread lingers as a tombstone instead of exiting, which had turned every later error into "connection closed".
- **A notification blocked behind a server that stopped reading had no backstop**: pending writes are now tracked and expired like requests. Found by deleting the crate-wide `allow(dead_code)`.
