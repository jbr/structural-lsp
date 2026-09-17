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
[servers.md](servers.md)). Work unrelated to this query counts — another
file's re-index, a `cargo check` — because an index that is still settling
answers differently from one that has settled, and nothing in the protocol
distinguishes the two.

The **only** timer is an inactivity backstop for a broken server, and it
measures silence, not duration: time since the server last sent *any*
message, reset by progress reports. A server still reporting progress is
busy, not broken. Default 90 s, configurable per server
(`backstop_secs`).

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

## Freshness comes from message ordering

Every file structural has touched is *owned*: it was `didOpen`ed, and is
re-pushed with `didChange` from disk before the query that reads it.
Language servers must apply a notification before handling a request that
follows it on the same connection, so a query issued after the push sees the
pushed text. No settle wait and no re-query.

Ownership is per running server and does not survive a fresh instance — a
restarted server owns nothing until something syncs to it again.

The known gap: a **bare-name query owns no file**, so `workspace/symbol` can
answer from an index that has not seen a recent external edit, and the
failure is silent (the honest-looking answer is "no such symbol"). Path-
scoped queries are current; bare-name ones are not yet. This is what units 4
and 5 of the discovery work exist to close
(`../backlog/bare-name-staleness.md`).

## Ranges are verified against the text just read

Every file an answer depends on is read from disk at the moment it is used,
and every range a server returns into that file is checked: the symbol's name
must sit there (or one of the adapter's reference aliases, such as `Self`).
A mismatch means the server's view of that file is stale, and it is an
**error** — never a retry, and never a listing built on it.

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
exactly once in the file and a focus exactly once within the anchor;
otherwise the error says where the occurrences are.

Candidates come from the servers' fuzzy symbol index — bounded, and **not
proven exhaustive** — except in a **file scope**, where a `path` naming a
file nominates from that file's own `documentSymbol` instead, so its
candidates are the file's own symbols rather than a sample of a bounded
cross-workspace response; the outline itself is not a proven-exhaustive
declaration index either (`resolution.md`). What is not proven is stated in
the tool descriptions and in the server's MCP instructions rather than
implied away.

## Degradation is named, never silent

When something is missing or capped, the answer says so in one line: a
listing cap and how many were dropped, a body budget and the bytes omitted, a
view a particular server does not support, a language no configured server
serves, a server that is `not started` / `starting` / `failed` with its
reason and its install hint. An error names the server and the last thing it
said.

## Every byte is paid for

The answer is read by a model that pays per token. One line per entry in a
listing; the happy path for `show` is one header line and the body; notes
appear only when something is degraded, one line each; caveats live once, in
the tool descriptions and the server's instructions, not in every result.
Current bounds: `show` body 12,000 bytes, listings 200 lines, `uses` 100
hits. Pagination does not exist yet
(`../backlog/uses-scope-and-paging.md`, `../backlog/outline-limits.md`).

## Mutation is the server's edit, applied faithfully

`action` never composes an edit itself: it asks the server for one, shows it
as a preview with a digest over the files it touches, and applies it only
when a later call presents that digest and nothing has changed underneath.
A server's own `workspace/applyEdit` is always declined (`{"applied":
false}`) — structural writes files, the server does not. There is no textual
post-pass over a semantic edit; if the server does not rewrite something, the
answer reports that rather than guessing.
