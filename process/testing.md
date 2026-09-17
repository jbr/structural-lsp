# Testing

```sh
cargo test                                   # unit + Python-mock tests
cargo test -- --ignored                      # rust-analyzer, pyright, gopls e2e
cargo clippy --all-targets -- -D warnings
cargo doc --no-deps
```

```sh
cargo test --test conformance -- --ignored --nocapture   # the scorecard
```

## The conformance probe measures servers, not this code

`tests/conformance/` is a measurement harness rather than a regression
suite. It asks every installed server of a language the questions `show`,
`uses` and `action` need — bare-name lookup, a file-scoped name, a
qualified name, cross-file references, an anchor to a local, a rename
preview — and prints a scorecard. It asserts only that it probed something:
**a low score is the finding, not a failure**, and a run that fails means
the harness broke.

What it measures is what a server *answered*, never what it advertised. An
`initialize` capability list is a claim, breadth of capability is
uncorrelated with quality on this narrow path, and the rest of this code
already treats a server's word as something to check against text it just
read.

Every cell is **one observation on the server and version that ran** — the
same standing as a pending acceptance case below — so the version is
recorded beside the score and a version that cannot be read is left blank
rather than guessed. Candidate argv comes from `reference/servers-list/`
(`scripts/reduce.py installed` prints it with a vote count per form), not
from us.

Reachability has three states and the middle one is the point: not on
`PATH`, on `PATH` and will not start, and runnable. The probe spawns the
real argv, because a broken shebang, a missing interpreter and a
non-executable file all pass a lookup. Even `runnable` is not availability
— that is settled by the handshake.

Four defects in `src/` came out of the first two runs
(`../journal/2026-09-17-conformance-probe-findings.md`). Pointing it at a
new language is a candidate list and a fixture.

## Tests are the instrument, not a formality

`src/test_support.rs` holds a Python mock language server that **records
every launch** and can stall its `initialize`. That is what makes the
ownership properties assertable at all: "concurrent demand causes one launch",
"irrelevant servers do not start" and "shutdown leaves no orphan" are
assertions about **pids**, not inspections of intent. Prefer extending that
mock to reasoning about the code.

Lifecycle races are picked with mock events, barriers and recorded pids —
**never timing sleeps**. Put an outer timeout on a process-lifecycle test so
a deadlock fails the run instead of hanging it. A passing happy path is not
evidence against a deadlock.

The front end is tested as a **real subprocess**. A shutdown/EOF bug once
passed every in-process host test while hanging the binary forever
(`../journal/2026-09-16-shutdown-escalation.md`); anything about how the
process reads stdin or handles signals has to be exercised through the
process.

Tests that mutate work on scratch copies. Acceptance cases should use
realistic common-name and multi-project inputs, not only tiny happy-path
fixtures — the qualified-name feature passed on a fixture and failed on a
real tree for exactly that reason
(`../journal/2026-09-16-file-scoped-nomination.md`).

Do not make `fixtures/` itself a root: rust-analyzer searches parent
directories for a `Cargo.toml` and will load this repository instead.

## Running them without losing an hour

Bound long runs with `gtimeout` and background them. **Do not call an
unbounded `wait`** — a stuck run cost an hour once. `cargo test -- --ignored`
spawns real servers and is slow.

## Pending acceptance cases

Some acceptance cases assert behavior that does not hold yet — they are
written for the unit that will make it hold. They are compiled only under
the `pending-acceptance` cargo feature (today: the bare-name freshness
cases in `tests/mcp_smoke/staleness.rs`), so `cargo test` and the standing
`cargo test -- --ignored` cannot run them — the cases are not even built —
and the suite stays green until they are asked for:

```sh
cargo test --features pending-acceptance --no-run          # compile check
cargo test --features pending-acceptance -- --ignored -- pending_
```

A failure keeps the case pending — that is the honest signal, not a broken
suite. A pass is one observation on the server and version that ran, not
validation and not a general guarantee: promote a case to ordinary ignored
coverage only when the unit that makes it hold has validated it per server,
with wording that says what was observed. When that unit ships, drop the
case's feature gate; the backlog entry goes with the unit.

## Understood flakes

The handle tests need a Python 3 that starts fast; `python3` behind a
version-manager shim can be too slow for the short backstops. They try a few
direct paths and honor `STRUCTURAL_TEST_PYTHON`. The timing tests' backstops
are three seconds with their ratios preserved, which is what they need under
contention — the machine is often shared with other agent sessions. If a
handle test fails alone, re-run before believing it.
