# The correctness contract

The rule every other doc in this tree inherits: **correct and current
information beats response time.** The calling agent has no sense of
wall-clock time and cannot tell a slow answer from a wrong one; the human
cares that the answer is right. What follows is what that costs, mechanism by
mechanism, and what would break if each were relaxed.

## No timer decides an answer

Before any semantic query, the server must have **no outstanding work**:
every `$/progress` token it opened has ended, and the adapter's readiness
policy agrees (`ready = standard_idle && policy.is_ready()`, see
[servers.md](servers.md)). A `window/workDoneProgress/create` opens a token
too, which is conservative beyond the protocol and is what a server that
reserves a token it never reports through opts out of
([servers.md](servers.md)) — without that opt-out such a connection is never
idle and this wait never ends. Work unrelated to this query counts — another
file's re-index, a build-script run — because an index that is still settling
answers differently from one that has settled, and nothing in the protocol
distinguishes the two.

The **only** timer is an inactivity backstop for a broken server, and it
measures silence, not duration: time since the server last sent *any*
message, reset by progress reports. A server still reporting progress is
busy, not broken. Default 90 s, configurable per server
(`backstop-secs`).

A busy server can also be a quiet one — rust-analyzer says nothing for
minutes while cargo builds proc-macros, with a progress token open — so the
transport sends a **liveness probe** after `probe_after` of silence with work
pending: a request under the `$/` prefix (`$/structural/ping`) that the spec
obliges the server to answer with `MethodNotFound`. The answer is activity
and extends every deadline. The backstop therefore means "silent *and* not
answering probes for this long."

A server that answers probes but never finishes is alive and stuck, which no
timer can distinguish from slow. That call blocks until the caller escalates
([shutdown.md](shutdown.md)); the boundary is deliberate, and it is the price
of never returning a partial answer on a timer.

What this rules out, permanently: settle sleeps, retry loops, "best result so
far", and any bound on a query that is progressing.

### An answer is accepted only from a ready window

"Ready when the request was written" is a claim about the moment of the
write, and a server can begin work after it. rust-analyzer applies a
change off its message stream: measured, its `quiescent: false` arrived 1
to 18 ms after the next request was already written, and a query that
lands while it re-scans its file roots is answered `null` — no error —
where its settled answer is the list. Mapped to "no results", that was a
silent wrong answer: in a tree with Rust and TypeScript both defining a
name, the bare name resolved to the TypeScript candidate alone. A wait on
each server in turn has the same hole one level up: a server can begin
work while the next one is waited for.

So the transport keeps a **readiness generation**: a count of the
transitions to not-ready it has observed — a `$/progress` `begin`, a
counted `window/workDoneProgress/create`, the readiness policy turning
(rust-analyzer's `quiescent: false`). Every semantic request — symbol
search, definition, declaration, references, implementation, call
hierarchy, hover, rename and prepare-rename, document symbols, an
adapter's extension views — goes through `ServerHandle::query`, which
writes it only while the server is ready and records the generation at
the write. A response that arrives under a different generation *while
the server is still not ready* was sent while work that began after the
write was outstanding: it is set aside, whatever it says, and the request
waits for readiness and is written again
(`src/lsp/handle/owner/query.rs`).

Work that began and ended before the response arrived does not set it
aside. A server reports its own state in order on its one stream, so
that response was sent by a server that had settled again. And the work
may be the request's own: pyright opens and ends a "Finding references"
progress token for every `references` it answers, before the answer. The
first version of this guard set aside any response under a moved
generation, and against the real servers it asked pyright the same
`references` question tens of thousands of times in a few minutes.

This is the readiness wait's own postcondition, not a retry loop. What
triggers it is a protocol signal of work, never the content of the
answer; it is not bounded by a count or a timer, because it ends exactly
when the server stops starting work that outlasts its answers, and a
server that keeps doing so is busy, which the backstop and the probe
already distinguish from broken. What it does not cover, stated: a
server that ends its request-scoped progress *after* sending the answer
would be asked again each time; none measured here does. It costs nothing for a server that stays idle: one integer
compared per response. For one that does start work in the window, the
cost is the wait a query issued a moment later would have paid, plus
one more request — including for unrelated work, a build-script run that
begins as the answer is being written, which is the same rule the wait
already follows. The responses set aside are kept in the event log, and
the count is in the connection's `Snapshot`.

The generation is not a substitute for the adapter's knowledge of what
an answer means. rust-analyzer's `null` to `workspace/symbol`,
`definition` and `implementation` is its answer while it is not loaded —
measured, its settled answer with nothing to report is `[]` — and that
is declared by its adapter (`Adapter::null_is_unloaded`,
[servers.md](servers.md)). Such a `null` arriving under an unchanged
generation is an **error** naming the server and saying its index is not
loaded, never "no results": there is no reported work left to wait for,
and reading it as empty is the silent wrong answer this section exists
to prevent. It is declared per server and per method from measurement;
`references` is not on the list, because rust-analyzer's settled answer
away from a name is `null` too.

Two mechanisms carry clocks here, and they are different kinds. The
inactivity backstop bounds a *query's* wait for a broken server. The idle
sweep ([observation.md](observation.md)) is a **resource-expiry** timer: it
releases a server process no strong signal has addressed for the
interval, and nothing waits on it — no query's latency and no answer's
content depends on when it fires. The rule above governs answers, and
stands unchanged: no settle sleeps, no timer-based claims of readiness.

## Freshness comes from message ordering

Every file structural has touched is *owned*: it was `didOpen`ed, and is
re-pushed with `didChange` from disk before the query that reads it.
Language servers must apply a notification before handling a request that
follows it on the same connection, so a query issued after the push sees the
pushed text. No settle wait and no re-query.

Ownership is per running server and does not survive a fresh instance — a
restarted server owns nothing until something syncs to it again.

A **bare-name query** owns no file, and neither does a directory scope, so
ownership alone cannot make them current. Every query therefore crosses
the **query boundary** first: each server that would answer is brought up
to the tree before any index is read.

Each server keeps a **census** — a stat (modification time and length) of
every file of its languages, walked as the evidence scan walks
([discovery.md](discovery.md)) — taken before its process is spawned and
re-taken at every boundary. A file created or written since the last
census is pushed with the text just read, exactly as a file-scoped query
pushes its file, and is owned from then on. A file gone from the census
is closed if owned and told to the server as deleted. There is no content
to push for a deletion, so whether the server's index has applied it by
the time it answers is the server's: rust-analyzer applies a deleted file
on its loader thread, after the query that follows the notice, and can
still nominate a symbol from it. A candidate whose file is not on disk is
therefore never navigated: it is excluded and counted, with a note that
the index has not yet applied the deletion. Taking the first
census before the process exists means nothing falls between the two:
what was written before it is on disk for the server's own first read,
and what was written after it is pushed.

Content, not a change notice, is what makes this message-ordering
freshness: the server holds the text before the query arrives, and
nothing waits on the platform's change delivery. A watcher-only boundary
was built first and measured short of this — FSEvents on macOS delivers
a write later than a query issued right after it
(`journal/2026-10-13-the-query-boundary-and-its-edge.md`). A stamp
cannot show a second write of the same length within one filesystem
tick, so a file whose stamp lies within two seconds of the census that
recorded it is not trusted and is pushed by the next one; once owned,
its content rather than its stamp is compared.

The watcher's delivered events are told to the servers after the
reconciliation, as `workspace/didChangeWatchedFiles` — reduced to each
path's state now, never for a document the server holds or for a path
under a directory the evidence scan skips, and re-stated in full after
the watcher reports lost events. That is what reaches the files no
census covers — manifests, configuration, files outside a server's
languages — and those ride on delivery: a `Cargo.toml` edited just
before a query may not be seen by it. The notification is sent on top
of each server's own watching, not instead of it: the client does not
offer the method for dynamic registration, because rust-analyzer,
offered it, registers and turns its own watcher off — leaving it only as
current as this client's delivery, which is no earlier and reaches it
only at a query.

What this does not establish, stated rather than implied away. Whether a
server's symbol search answers from a document it was just handed is the
server's: validated on rust-analyzer and pyright
(`tests/mcp_smoke/staleness.rs` — an edit to an unopened file, a new
file, and a deleted file, each followed immediately by a bare-name
query), and not argued from the spec for any other. The census stops at
the evidence scan's file budget, so in a tree past it an edit beyond the
budget reaches the server only by its own watching; a miss in such a
tree says so. The cost is a stat walk per answering server per query,
measured at 3 ms for this repository's 133 Rust files and 190 ms for a
4,500-file Rust tree.

## Ranges are verified against the text just read

Every file an answer depends on is read from disk at the moment it is used,
and every range a server returns into that file is checked: the symbol's name
must sit **within** it (or one of the adapter's reference aliases, such as
`Self`). A range holding no such name means the server's view of that file is
stale, and it is an **error** — never a retry, and never a listing built on
it.

Within, not equal to. `SymbolInformation.location.range` is specified as the
range enclosing the symbol and has no `selectionRange` to carry the name
separately, so a server may report a whole declaration
(`def choose(flag: bool) -> int:`) where another reports just `choose`; both
are conforming, and the tight range is a convention. The name is located
inside the reported range at a token boundary, and that position — not the
range's start — is what a follow-up `definition` or `references` request is
sent at. Two occurrences of the name in one range is ambiguous and refused,
because nothing ambiguous is silently picked.

A **rename edit** is checked strictly, and that is not an inconsistency: an
edit replaces its range, so a range wider than the name would rewrite the
surrounding code. There, a mismatch really is the server's view disagreeing
with the file.

An **outline container's** selection range gets the same within-not-equal
rule, for the same reason arriving from the other shape: R's
`languageserver` selects `Config <- setRefClass(` for a node it advertises
as `Config`. The advertised name is located in the selected text at a token
boundary, which is also what keeps `Item` from matching a range naming
`Item2`. Several occurrences is refused as everywhere else — and R's own
fixture produces exactly that, since `Config <- setRefClass(` reaches the
`"Config"` argument on the next line. The refusal says what was advertised
and what was read and names no cause: a range that does not name the node
and one that names it twice both arrive there, and the wording that picked
one was wrong for the other.

Where the check happens depends on what the candidate is for. A
**file-scoped** candidate is checked when it is nominated, on the text
already in hand, before a qualifier filter or a listing can use it, and its
range is what every listing of it prints. An **index-nominated** candidate
is checked when the pipeline navigates from it (`definition_of`), so a
listing built without navigating — the unqualified and fuzzy listings —
carries the ranges the index reported, which is one reason the index is
described as bounded and unproven rather than authoritative
(`resolution.md`).

Position conversion is honest in both directions and rejects rather than
clamps: a byte offset off a character boundary, a line past the end, a column
past its line, or a column inside a surrogate pair is an error, not a
silently moved cursor (`src/lsp/positions.rs`).

## Nothing is silently picked

`workspace/symbol` — or, in a file scope, the file's own outline — only
*nominates*. Every exact candidate goes to
`textDocument/definition`, and results are deduplicated by target: hits that
navigate to the same place are one identity, hits that navigate elsewhere are
listed as ambiguous with their kinds, paths and signatures. Zero exact
matches produce a fuzzy "similar:" listing with kinds, not a wrong pick.
Anchors are subject to the same rule in the text domain: an anchor must occur
exactly once in the file and a focus exactly once within the anchor (on
identifier boundaries, else as a substring: `resolution.md`); otherwise the error says where the occurrences are.

Candidates come from the servers' fuzzy symbol index — bounded, and **not
proven exhaustive** — except in a **file scope**, where a `path` naming a
file nominates from that file's own `documentSymbol` instead, so its
candidates are the file's own symbols rather than a sample of a bounded
cross-workspace response; the outline itself is not a proven-exhaustive
declaration index either (`resolution.md`). What is not proven is stated in
the tool descriptions and in the server's MCP instructions rather than
implied away.

A server that truncates its symbol search declares its cap
(`Adapter::symbol_result_cap`). An answer that reaches the cap without
naming the symbol is a prefix of the matches, so a miss in it is not
absence and an exact match from another server beside it is not proven
to be the only one: the first is listed as a miss *among the matches the
index returned*, the second as one definition that may not be the only
one, each with a note naming the server and its cap and pointing at a
file scope, which resolves from the file's outline instead
(`resolution.md`).

## Degradation is named, never silent

When something is missing or capped, the answer says so in one line: a
listing cap and how many were dropped, a body budget and the bytes omitted, a
view a particular server does not support, a language no configured server
serves, a server that is `not started` / `starting` / `failed` with its
reason and its install hint. An error names the server and the last thing it
said.

## Every byte is paid for

The answer is read by a model that pays per token. One line per entry in a
listing; the happy path for `definition` is one header line and the body; notes
appear only when something is degraded, one line each; caveats live once, in
the tool descriptions and the server's instructions, not in every result.
Current bounds: `definition` body 12,000 bytes, listings 200 lines, `references` 100
hits. Neither listing that can run long ends in a bare count: an `outline`
is paged, 200 lines a page, and a capped `references` listing summarizes its
remainder by directory; each names the `page` that continues it
([tools.md](tools.md)).

## Mutation is the server's edit, applied faithfully

`rename` never composes an edit itself: it asks the server for one, shows it
as a preview with a digest over what the preview shows, and applies it only
when a later call presents that digest and the recomputed edit, and the
lines it changes, are still the ones previewed ([tools.md](tools.md)).
A server's own `workspace/applyEdit` is always declined (`{"applied":
false}`) — structural writes files, the server does not. There is no textual
post-pass over a semantic edit; if the server does not rewrite something, the
answer reports that rather than guessing.
