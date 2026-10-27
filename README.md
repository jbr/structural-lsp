# structural-lsp

[![ci][ci-badge]][ci]
[![crates.io version][version-badge]][crate]
[![docs.rs][docs-badge]][docs]
[![codecov][codecov-badge]][codecov]

[ci]: https://github.com/jbr/structural-lsp/actions?query=workflow%3ACI
[ci-badge]: https://github.com/jbr/structural-lsp/workflows/CI/badge.svg
[version-badge]: https://img.shields.io/crates/v/structural-lsp.svg?style=flat-square
[crate]: https://crates.io/crates/structural-lsp
[docs-badge]: https://img.shields.io/badge/docs-latest-blue.svg?style=flat-square
[docs]: https://docs.rs/structural-lsp
[codecov-badge]: https://codecov.io/gh/jbr/structural-lsp/graph/badge.svg
[codecov]: https://codecov.io/gh/jbr/structural-lsp

An [MCP](https://modelcontextprotocol.io) server that gives coding agents
LSP-backed code intelligence: `definition`, `references` and `rename`,
answered by the language servers an editor would use (rust-analyzer, pyright,
gopls, clangd, and others) rather than by grep. A target is named the way a
model would name it — `Config::new`, or a quoted line of source — and never by
line number, since a model's copy of a file may be several edits old. Before
each query, every file changed on disk is pushed to the servers and the
servers are waited on, so the answer reflects the tree as it is now.

One process can serve several agents: a host running many agents, each in
its own working directory, shares one set of language servers per project
instead of starting a set per agent.

## What a call looks like

These are the tools run from the command line against this repository; over
MCP, the same arguments arrive as JSON and the same text comes back.

```text
$ structural-lsp definition AgentId
struct AgentId · src/host.rs:47
/// Which agent — or front end — a workspace binding belongs to. Agent start
/// and end arrive as these identifiers, so releasing one agent's interest
/// never releases another's.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AgentId(String);
```

```text
$ structural-lsp rename --symbol Session::warm --new-name prewarm
rename warm → prewarm · fn warm · src/session.rs:168 · 2 edits · 1 file
src/session.rs
  85:         session.warm(None);
              session.prewarm(None);
  168:     pub fn warm(&self, scope: Option<&Path>) {
           pub fn prewarm(&self, scope: Option<&Path>) {
note: rust-analyzer rename may leave intra-doc links unchanged; check doc comments for `warm`.
digest: 32e6f7d2dc7e21e5
```

A target that does not resolve to exactly one definition is answered with
the candidates, not a guess:

```text
$ structural-lsp references Session::root
no 'root' is in a container ending in `Session`; the 'root' candidates:
fn root · src/discovery/roots.rs:212 · in Ancestry
fn root · src/workspace.rs:226 · in Workspace
fn root · src/lsp/server.rs:212 · in LspServer
fn root · src/host.rs:190 · in Entry
…
```

## Tools

| tool | what it does |
| --- | --- |
| `definition` | A definition's source, with its doc comments and attributes. `view` selects another view of the same target: `outline`, `implementations`, `incoming-calls`, `outgoing-calls`, `type` (hover), and per-server views such as rust-analyzer's `expanded` (macro expansion) and `related-tests`. |
| `references` | Every reference, one line each with its enclosing item, grouped by file, tests listed separately. `within` narrows the listing to a directory; long listings page. |
| `rename` | Previews every changed line and returns a `digest`; `mode = "apply"` with that digest recomputes the edit and writes it only if nothing it touches has changed since. |

All three take their target the same way:

- `symbol` — a name, bare (`choose`) or qualified by its container or module
  (`Config::new`, `listing::render`). Only exact names resolve.
- `anchor` + `path` — source text quoted from a file, reaching what a name
  cannot: a local, a call site, an expression. `focus` names the token within
  the anchor that is meant.
- `path` scopes the lookup to a file or directory; `workspace` answers about
  another project for one call; `language` (`rust`, `python`, `ts`, …) asks
  only that language's servers.

The full argument reference is `structural-lsp <tool> --help`, which prints the
same descriptions an MCP client receives.

## Installation

Prebuilt binaries are published for macOS and Linux, on x86_64 and aarch64.
With [cargo-binstall](https://github.com/cargo-bins/cargo-binstall):

```sh
cargo binstall structural-lsp
```

Or with the installer script, which downloads the binary for your platform
into `~/.cargo/bin`:

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/jbr/structural-lsp/releases/latest/download/structural-lsp-installer.sh | sh
```

Or from source:

```sh
cargo install structural-lsp
```

Archives for each platform are on the [releases page](https://github.com/jbr/structural-lsp/releases).

structural-lsp does not install language servers. It starts the ones it finds on
`PATH`, so install the server for each language you want answered — for
example `rustup component add rust-analyzer`, `npm install -g pyright`, or
`go install golang.org/x/tools/gopls@latest`. When a language's preferred
server is missing, the next one in [the catalogue](#languages) is tried, and
an answer that needed a missing server says so with an install hint.

## Using it from an MCP client

`structural-lsp serve` speaks MCP over stdio. The workspace is the project
containing the directory the server was started in (or `STRUCTURAL_ROOT`, if
set): it walks up from there to the enclosing project root, so an
agent started in a subdirectory of a cargo workspace shares that workspace's
servers.

**Claude Code:**

```sh
claude mcp add structural -- structural-lsp serve
```

(`--scope user` adds it for every project.)

**Other clients** (Claude Desktop, Cursor, gemini-cli, …) take an entry like:

```json
{
  "mcpServers": {
    "structural": {
      "command": "structural-lsp",
      "args": ["serve"]
    }
  }
}
```

Language servers start when structural-lsp does, but the MCP handshake does not
wait for them; the first tool call does. Facts an agent cannot act
on — a server that would not start, a fallback taken, a language nothing
serves — are written to stderr and sent to the client as MCP log messages.

## Languages

Which server is tried first for a language is decided by measurement: each
candidate is asked the questions these three tools depend on, against a
fixture project, and the catalogue orders candidates by how many they
answered. [docs/catalogue.md](docs/catalogue.md) has the scores, the server
versions they were measured on, and the candidates left out.

| language | servers, in the order tried |
| --- | --- |
| Rust | rust-analyzer |
| Python | pyright, basedpyright, jedi-language-server, pyrefly, zuban, ty, pylsp, pylyzer, … |
| TypeScript | vtsls, typescript-language-server, deno, … |
| Go | gopls |
| C++ | clangd, ccls |
| Ruby | solargraph, ruby-lsp, … |
| Erlang | elp, erlang_ls |
| Bash, R, Gleam, QML, Swift, Elm | one candidate each |

Any other language server can be added in `structural.toml`.

## Configuration

structural-lsp needs no configuration. A `structural.toml` at the project root
overrides the catalogue: it can change a built-in server's command or
inactivity backstop, disable a server for this project, or add a server the
catalogue does not know.

```toml
#:schema https://raw.githubusercontent.com/jbr/structural-lsp/main/schemas/config.json

idle-eviction-secs = 1800        # stop a server no query has used for this long

[servers.rust-analyzer]          # a built-in's name overrides it
command = ["/opt/ra/rust-analyzer"]

[servers.zls]                    # any other name adds a server
command = ["zls"]
languages = ["zig"]

[servers.pyright]
disabled = true
```

The `#:schema` line gives editors with [Taplo](https://taplo.tamasfe.dev) or
[Tombi](https://tombi-toml.github.io/tombi/) completion and hover text for
every key. Unknown keys are an error, and a file that does not parse starts
nothing: every call reports the error rather than running a configuration
other than the one written. The full reference is
[docs/servers.md](docs/servers.md).

## Correctness

structural-lsp does not trade a correct answer for a fast one. Before a semantic
query, it waits until the server reports no outstanding work — including
indexing unrelated to the query — and an answer that arrives after the server
began new work is asked again. There are no settle sleeps and no partial
answers on a timer; the only timer is an inactivity backstop for a server
that has stopped responding, and a silent server is probed before that
backstop can trip. Every range a server returns is checked against the file
as just read from disk, and a mismatch is an error rather than an answer.

The consequence is that the first query in a large project waits for the
server's initial index. While a call waits, it sends MCP progress
notifications saying what it is waiting on.

The contract, and what each mechanism costs, is
[docs/correctness.md](docs/correctness.md).

## Embedding

The crate is also a library. A harness that runs several agents creates one
`Host`, binds each agent to its working directory with
`Host::agent_started(AgentId, &Path)`, and calls the tools on the `Session`
that returns; agents in the same project share servers, and
`Host::agent_ended` releases one agent's interest without touching
another's. `Host::with_config_source` replaces reading `structural.toml` with
a function returning a typed `Config`, and `Host::with_operator` redirects
the operator events that otherwise go to stderr. This surface is not yet
stable. [docs/ownership.md](docs/ownership.md) describes the model.

## Development

[docs/README.md](docs/README.md) is the top of the architecture docs: how a
call flows, the invariants, and a map from each subsystem to its doc and
modules. [CHANGELOG.md](CHANGELOG.md) records what has shipped, and
[CONTRIBUTING.md](.github/CONTRIBUTING.md) how a change reaches this
repository.

```sh
cargo test                                   # unit and mock-server tests
cargo test -- --ignored                      # end to end against rust-analyzer, pyright, gopls
cargo clippy --all-targets -- -D warnings
```

[process/testing.md](process/testing.md) covers what the suites need
installed and the conformance harness behind the catalogue.

## License

<sup>
Licensed under either of <a href="LICENSE-APACHE">Apache License, Version
2.0</a> or <a href="LICENSE-MIT">MIT license</a> at your option.
</sup>

<br/>

<sub>
Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this crate by you, as defined in the Apache-2.0 license, shall
be dual licensed as above, without any additional terms or conditions.
</sub>
