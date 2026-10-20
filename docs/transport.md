# The transport: one language server, three threads

`src/lsp/handle.rs` (+ `src/lsp/handle/`) runs one language server as a child
process and exposes a blocking `ServerHandle`. Underneath it are three pure
modules with no I/O: `src/lsp/framing.rs` (the wire), `src/lsp/jsonrpc.rs`
(classification) and `src/lsp/handlers.rs` (replies to server-initiated
requests).

Nothing here waits for a tool call to make progress, and nothing blocks a
caller in order to keep the protocol moving.

## The threads

- **reader** — turns the child's stdout into frames and pushes them,
  blocking, into the owner's inbox. Backpressure lands on the child, never on
  a caller.
- **owner** — holds *every* piece of protocol state (pending requests, open
  progress tokens, the readiness policy, the diagnostic event log), answers
  server-initiated requests, and is the only thread that touches any of it.
- **writer** — owns the child's stdin, so wire order is enqueue order and a
  server that stops reading blocks only this thread.

Callers hold a `ServerHandle` (every method takes `&self`) and block on
`request`, `notify` and `wait_ready`. When idle the owner wakes at most
`CONTROL_TICK` (250 ms) apart to re-check the terminate flag and overdue
operations: a control-plane wake-up, not a settle delay. Nothing is processed
on a tick that carried no message, and a readiness wait completes on the
message that satisfies it, never on a tick.

## The one timer

Every blocking call is bounded by exactly one timer, the **inactivity
backstop**. It starts when the operation is admitted, is reset by every
message the server sends (progress reports included), and trips only when the
server has been silent that long. A **liveness probe** (`$/structural/ping`,
under the `$/` prefix so the spec's `MethodNotFound` obligation applies) is
sent after `probe_after` of silence with work pending; one probe is
outstanding at a time. The reply is activity like any other. So the backstop
means "silent *and* not answering probes for this long", and a server that
answers probes but never finishes is alive and stuck — see
[correctness.md](correctness.md) for why that is the deliberate boundary.

## The readiness generation

The owner counts every transition to not-ready it observes, comparing
readiness before and after each message it applies. `ServerHandle::query`
is the semantic request: held in the owner (`owner/query.rs`) until the
server is ready, written with the count current at the write, and its
response set aside — into the event log, the request held to be written
again — when the count has moved and the server is still not ready as
the response arrives; work that began and ended before it does not. Held requests count as
pending operations and are bounded by the backstop like a readiness
wait. `request` is the unguarded path, used for `initialize`, and the
`Snapshot` carries the count and how many answers were set aside
([correctness.md](correctness.md)).

`STRUCTURAL_WIRE=1` traces every frame to stderr, each incoming one with
its body truncated to 240 characters, and each set-aside answer.

## Bounds

Every queue is bounded and admission never blocks a caller. The caps in
`Settings` count messages, frames, operations and tokens — not bytes; a
single frame is bounded by `framing::MAX_FRAME_BYTES` inbound and by the
caller outbound.

| setting | default | bounds |
| --- | --- | --- |
| `inbound_cap` | 1024 | messages queued for the owner (server frames and caller commands share it) |
| `writer_cap` | 64 | frames queued for the writer |
| `pending_cap` | 256 | admitted operations not yet finished |
| `progress_token_cap` | 4096 | progress tokens open at once; a server past this is treated as broken |
| `event_cap` | 2000 | diagnostic event-log entries retained, oldest dropped |
| `backstop` | 90 s | the inactivity backstop |
| `probe_after` | 20 s | silence, with work pending, before a probe |
| `shutdown_grace` | 5 s | graceful `shutdown`/`exit` before the child is killed |

A caller whose operation cannot be queued gets an error naming the queue. The
one place a full queue is fatal is a **required** frame — the reply to a
server-initiated request — which is never dropped silently; the connection is
terminated instead.

`Snapshot` is the routine readout (label, `ready`, open tokens, pending
operations, a one-line summary of the last message and how long ago, and the
failure reason once a connection has failed). It deliberately does not carry
the event log; `capture_events` is the diagnostic path.

## Framing and classification

LSP stdio framing is `Content-Length` header blocks around JSON bodies.
mcplease has no LSP framer — its `stdio` is newline-delimited MCP — so this
pair is hand-written. Both directions bound allocation, and the header block
is read one byte at a time so the caps apply *before* any body is sized from
a hostile `Content-Length`.

`jsonrpc.rs` classifies a frame before serde collapses shapes. The
non-obvious case: an explicit `"result": null` is a valid empty answer, while
a response with no `result` member at all is malformed. serde maps both to
`None`, so the distinction is captured from the raw object first.

## Replies to the server

`handlers.rs` decides the reply to each server-initiated request as a pure
function with no I/O, so the policy is testable without a process; the
transport only serializes what it returns. `workspace/applyEdit` is **always**
declined with `{"applied": false}`, whatever edit it carries — structural
applies edits itself, from results it asked for ([tools.md](tools.md)).

## Teardown

`ServerHandle::shutdown` and `Drop` both set a terminate flag the owner
checks on every wake *and* try to queue a graceful shutdown command: the flag
is the reliable path when the inbox is full, the command the fast one. Both
converge on the sequence in [shutdown.md](shutdown.md). A worker thread that
cannot be joined within `JOIN_GUARD` (2 s) is detached — a reader can be
stranded by a grandchild that inherited the server's stdout, and the thread
holds nothing but a dead pipe.
