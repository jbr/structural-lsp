# Testing

```sh
cargo test                                   # unit + Python-mock tests
cargo test -- --ignored                      # rust-analyzer, pyright, gopls e2e
cargo clippy --all-targets -- -D warnings
cargo doc --no-deps
```

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
real tree for exactly that reason (`../backlog/file-scoped-lookup.md`).

Do not make `fixtures/` itself a root: rust-analyzer searches parent
directories for a `Cargo.toml` and will load this repository instead.

## Running them without losing an hour

Bound long runs with `gtimeout` and background them. **Do not call an
unbounded `wait`** — a stuck run cost an hour once. `cargo test -- --ignored`
spawns real servers and is slow.

## Understood flakes

The handle tests need a Python 3 that starts fast; `python3` behind a
version-manager shim can be too slow for the short backstops. They try a few
direct paths and honor `STRUCTURAL_TEST_PYTHON`. The timing tests' backstops
are three seconds with their ratios preserved, which is what they need under
contention — the machine is often shared with other agent sessions. If a
handle test fails alone, re-run before believing it.
