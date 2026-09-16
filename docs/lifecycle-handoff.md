# Handoff: the lifecycle and discovery work

Written 2026-09-15 at the end of a long session, for whoever picks this up next
— probably me, cold. It assumes you have read nothing. It is deliberately
about *state and reasons* rather than narrative: what exists, why it looks like
that, and what to do first.

## What this is

`structural` is a Rust MCP server giving coding agents LSP-backed code
intelligence (`show`, `uses`, rename) from real language servers, and it is
meant to be embedded in a host process running several agents that share those
servers. The governing plan is **`docs/lifecycle-and-discovery.md`** — read it
first; it is the authority on intent, and it sequences six units.

## Where the work stands

| Unit | State |
|---|---|
| 0 — record the contract, regressions | the document exists; its residue (C1 file-scoped lookup fix, C2 regressions, the freshness experiment) is **open** |
| 1 — extract the shared host | **done**, plan in `docs/host-extraction.md` |
| 2 — explicit server lifecycle | **done**, plan in `docs/server-lifecycle.md` |
| 3 — discovery and selection, pure policy | **planned, not built**: `docs/discovery-and-selection.md` |
| 4, 5, 6 | not started |

Commits, oldest first:

- `5b3e903` unit 1: `State` → `Workspace`, `Host`/`Entry`/`Session`, the released
  registry.
- `714b854` unit 2's plan; `2351961` adapters become `Arc<dyn Adapter>`.
- `e3af7a4` launches get their own threads; stopping a server = removing it and
  marking the slot cold.
- `8a7ccb2` a flake fixed by construction; `84e3d1d` test backstops raised.
- `f329674` shutdown reaches a child that is still starting; `8050120` the
  launch count survives a panic; `f20c21c` swansong replaces that count;
  `b7efc58` registration refused once stopping; `c199ff6` claim and initiation
  are one operation; `8b40c9f` election refuses after the claim, and the sweep
  reaps outside the lock; `3244014` publishing is bounded.
- `ccd1849` and `282b77f` corrected unit 2's plan to match what was built.
- `4179a77` unit 3's plan; `bb2919a` its framing corrected; `dd05a5b` the
  outside-reviewer brief.

## Unit 2's design, and the reasons that must not be tidied away

This is the part to read before changing anything in `src/host.rs`. Each
sentence here is load-bearing and each has a comment in the code.

**Shape.** `Host` owns `workspaces: Mutex<BTreeMap<canonical root, Arc<Entry>>>`
plus a `released` registry of weak references to workspaces a session still
holds after their agents released them. An `Entry` owns the root, the workspace
behind its own mutex, the interested agent set, the slot handles, a registry of
children being started, and one shared `Lifetime`.

**Lifetime.** `Lifetime { song: Swansong, stopped: Arc<AtomicBool>,
publish_bound }` is cloned into every entry — *shared* state, not a copy, which
is what makes "one claim" true. `Lifetime::stop()` sets the claim **and**
initiates the shutdown: they are one decision, and separating them once cost an
afternoon of hanging.

**Launch path.** `Entry::activate` elects and *starts* every needed launch
before waiting on any, so latency is the slowest server rather than the sum.
Each launch runs on its own thread, holds no lock while it spawns and
initializes, and publishes in a single workspace-lock hold. `Launch` (in
`src/workspace/slot.rs`) owns its slot and leaves `Starting` on every exit path
*including a panic*; waiters wake on every terminal state, never only on
`Running`.

**The child registry.** A launcher registers its child — a `Stopper`, which is
the ability to say stop and nothing else — as soon as it spawns, so a shutdown
can kill one that no workspace holds yet. `register_child` refuses once the
lifetime is stopping, **and that check is inside the children lock on purpose**:
reading the flag first and then taking the lock reopens the window it closes.

**Shutdown, four passes.** Claim the lifetime → stop the children that are
starting → wait for completion (every launch's guard) → sweep: take each
workspace's servers out under its lock and reap them *outside* it.

The invariant: **every spawned child is either published and stopped by the
sweep, or killed by the launcher that spawned it.** It holds because the claim
is set before the sweep and re-read inside the same workspace-lock hold that
publishes, so a launcher publishing after the sweep would have had to see a
clear flag that no longer existed. The guards carry the *return* condition, not
the invariant.

**Two properties that look like details and are not.** The shutdown wait is what
makes the sweep's `Cold` **final** — a launch returns its guard only after it
has written to its slot, so no slot write can land after a `mark_cold`; bounding
that wait would lose it quietly. And a launch that cannot publish inside
`DEFAULT_PUBLISH_BOUND` stops the child it started rather than queueing for a
lock a wedged query may hold indefinitely; the bound is a **parameter on the
host** (`Host::with_publish_bound`) because what it trades is visible from
outside. `std::sync::Mutex` has no timed acquisition — there is no
`try_lock_for` — so that bound is a poll, deliberately.

**Known limits, documented rather than fixed.** A workspace acquired after
shutdown begins is covered by the shared claim, so nothing starts in it. Two
concurrent `shutdown()` calls can let one return while the other is still
stopping servers — unreachable in the single-threaded MCP path, and the
precondition an embedded consumer will violate. And `panic = "abort"` would
turn the panic-safety of the guards into silence, which is worth two lines in a
module doc.

## Open work, in the order I would take it

1. **The outside review (blocked on a harness bug).** `docs/astra-briefing.md`
   briefs a stronger model on unit 3's plan, open-ended, with the falsifiability
   question asked directly and an invitation to rewrite the plan after talking
   to me. The session is `proud-reef` and it cannot run: the codex connection
   authenticates with an OAuth refresh dance and the harness caches the token it
   booted with, so the first request 401s with zero tokens used. The operator is
   fixing that. When it works: it has **no `sessions__*` tools**, so it cannot
   message me — it will write to `docs/astra-review-unit-3.md` and I read its
   transcript. `sessions__notify` reaches it; `sessions__turn` does not (see
   below).
2. **The generation-counter question**, folded into that brief as a separable
   unit-2 mechanism question: at publish a launcher asks "is the host stopping?"
   where it may need to ask "is this slot still the slot I was starting a server
   for?" — a per-slot generation bumped whenever the slot is stopped, carried in
   the guard, compared at publish. It would subsume the flag read at publish
   time. **Open, and specifically worth an outside opinion**: I could not
   construct the interleaving that makes it bite (an evicted slot is `Running`,
   a launching one `Starting`), and I have been confidently wrong twice in this
   exact area. Not implemented; unit 3 does not evict, so nothing is broken by
   waiting.
3. **Unit 2's remaining test gaps**, all found by review and none of them
   speculative: a launcher parked on the workspace lock (partly covered now by
   the publish-bound test), two concurrent `shutdown()` calls, and
   `LspServer::spawn`'s `Err` paths — which are safe *only* because `Spawning`
   owns the handle and its `Drop` reaps, which is load-bearing and unasserted.
4. **Unit 3, milestone A.** Selection as a pure function over evidence and a
   catalogue, returning plans with reasons; fixture-tree tests asserting the
   plans. **This needs no dataset work at all** — the fixtures carry explicit
   catalogues. Then the catalogue generator, which consumes the operator's
   synthesis, not the raw dataset.
5. **Step 0's residue**, independent and parallel: the C1 file-scoped lookup fix
   (`show`/`uses` should nominate from the file's document-symbol tree rather
   than filtering workspace-symbol results), the C2 pending regressions, and the
   freshness/watch experiment — that last one is the operator's to run.
6. **Small doc item:** the parent plan and unit 2's both name "stopping" among
   the lifecycle states; the tree has four (`Cold | Starting | Running |
   Failed`) and does not need a fifth. Amend one or the other rather than the
   enum.

## How to work here

- **Verification:** `cargo test`, `cargo clippy --all-targets -- -D warnings`,
  `cargo doc --no-deps`. `cargo test -- --ignored` runs real servers and is
  slow. Bound long runs with `gtimeout` and background them — **do not call an
  unbounded `wait`**, which is how a stuck run cost an hour once.
- **The machine is shared** by several agent sessions, so the suite can flake
  under load. The flakes are understood: the timing tests' backstops were too
  tight for a python interpreter to start under contention, and they are now
  three seconds with the ratios preserved. If a handle test fails alone, re-run
  before believing it.
- **Tests are the instrument.** `src/test_support.rs` holds a Python mock that
  records every launch and can stall its `initialize`, so "one launch",
  "irrelevant servers do not start" and "no orphan" are assertions about pids
  rather than about intent. Prefer that over inspection.
- **Conventions.** Files stay under ~500 lines; no `#[allow(dead_code)]` per item
  — delete or use; server-specific strings live in `src/lsp/adapter/`; the only
  timers are the inactivity backstop, unit 2's idle sweep, and the publish
  bound above. No settle sleeps, no retry loops, no partial answers on a timer.
- **Reviewers**, all reachable and all useful: `host-review` (glm) and
  `cold-frame` (qwen) exist and have been worth their tokens — between them they
  found a shutdown hole, a race, a reachable panic, an ordering bug, and two
  doc-rot instances. Neither has `sessions__*` tools, so **they answer as text
  in their own turn and you read the transcript**; do not ask them to message
  you. If you need to reach one, `sessions__notify` works on a session you did
  not create, `sessions__turn` does not (a mutating call reaches only sessions
  the caller created). Ask for *questions* rather than answers when you want a
  frame attacked, and tell them not to consolidate their context if you expect
  more than one exchange.
- **Review technique that has paid repeatedly:** ask a reviewer to *try to break
  a specific invariant* rather than to review in general; ask for a *falsifier*
  rather than an opinion (the first unit-1 defect was proved by a scratch crate,
  not argued); and when a claim about a dependency or a platform is in question,
  **measure it in-repo** rather than reading the source — the guard-after-stop
  behaviour that closed the registration window was found that way, and reading
  the dependency's source would not have told me how it affected *our* shutdown.

## What I would tell myself, cold

The two failures in this work that cost the most were both *liveness*, both in
shutdown, and both invisible to every test I had at the time: a count returned
by an ordinary call that a panic skips, and a wait for a completion that was
never started. Both are now structurally prevented — by a drop-based guard, and
by making the claim and the initiation one operation — and both were found by
reviewers, not by me. The pattern is that shutdown's failure mode is *silence*,
so its code deserves more suspicion than its tests do.

The second pattern: the code and the plan drifted apart twice, and each time a
reviewer caught it. When you change a mechanism, grep the prose for its old
description — "the map lock is held across startup", "this whole policy is
milestone A's", "the sweep stops servers while holding the lock" all described
behaviour that had been deleted.
