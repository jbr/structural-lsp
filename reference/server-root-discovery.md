# Servers discover their own project root

Measured 2026-09-18 against rust-analyzer and gopls on purpose-built
fixtures, to settle whether a client must compute a project root before
starting a server or can hand over a working directory and let the server
find the project itself.

**Both servers walk up.** Told a directory well inside a project, each
located the enclosing project and answered about files *outside* the
directory it was given.

## Method

Two fixtures, each queried through structural's own front end with
`STRUCTURAL_ROOT` set to successively deeper directories, so what varied was
only the root the server was started against.

**Cargo workspace.** `Cargo.toml` (`[workspace]`, members `crates/alpha` and
`crates/beta`) at the top; `Widget` defined in `crates/alpha/src/lib.rs` and
used from `crates/beta/src/lib.rs` through a path dependency.

**Go module.** `go.mod` at the top, `NewWidget` defined in
`internal/thing/thing.go`, called from `main.go`.

| root the server was given | references found |
| --- | --- |
| `wsprobe` (workspace root) | 6, across `crates/alpha` and `crates/beta` |
| `wsprobe/crates/alpha` | 6 found, 3 reported — 3 filtered as outside the root |
| `wsprobe/crates/alpha/src` | 6 found, 3 reported — same |
| `goprobe` (module root) | 1, in `main.go` |
| `goprobe/internal/thing` | 1 found, 0 reported — 1 filtered as outside the root |

## What it establishes

**The server is not the limiting factor.** rust-analyzer started against
`crates/alpha/src` — a directory holding no `Cargo.toml` — nevertheless
loaded the whole cargo workspace. The proof is that its answer *contained*
`crates/beta`'s references: they were present in the reply and removed
afterwards by the client's own out-of-root filter. gopls behaved the same
way from `internal/thing`.

**A server told a subdirectory indexes the whole project anyway.** So two
clients rooted at two sibling members of one cargo workspace do not each
index half of it; they each index all of it. Whatever a narrower root buys,
it is not a smaller index.

**The narrowing is the client's.** In these runs the filtering was
structural's (`docs/resolution.md`), and it said what it had dropped. A
client that filters silently would present a project-shaped hole as an
absence.

**rust-analyzer's chosen boundary was the topmost manifest, not the
nearest.** From `crates/alpha`, the nearest `Cargo.toml` is the member's;
the workspace it actually loaded is the outermost one. That agrees with
Helix's documented rule (topmost marker between the file and the workspace)
and disagrees with nvim-lspconfig's (nearest ancestor carrying the
highest-priority marker) — see [`servers-list/`](servers-list/) for the six
conventions and how they differ.

## Limits

Two servers, two languages, small synthetic trees, one machine, one day.
It establishes that upward discovery *happens* in the two cases most likely
to need it; it does not establish that every server does it, how deep any
server will search, or what any of them does when markers nest ambiguously.
A server that does *not* walk up is a fact about that server, and belongs in
its catalogue entry rather than in a system-wide mechanism.

Not measured: whether a server told a root *above* its project over-indexes
— pointing rust-analyzer at a VCS root containing several unrelated cargo
projects may cost more than pointing it at one of them. That is the reason a
root rule bounded by the VCS root is not the same as a root rule that *uses*
the VCS root.
