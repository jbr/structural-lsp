# structural

An MCP server that gives coding agents LSP-backed code intelligence. Design in
`design.md`; the current plan is `docs/slice-2.md` with decisions recorded in
`docs/slice-2-handoff.md`.

## Conventions

- **Files stay under about 500 lines.** A soft limit, not a rule to game:
  when a file crosses it, split by responsibility in the same change. Tests
  go in a sibling module (`foo.rs` + `foo/tests.rs`), rendering apart from
  logic, helpers apart from the flow that uses them.
- Correct and current beats fast. No settle sleeps, retry loops, or partial
  answers on a timer; the only timer is the inactivity backstop
  (`docs/slice-2.md`, "Governing rule").
- Server-specific strings and methods live in an adapter, never in the
  generic transport.
- No `#[allow(dead_code)]` per item; delete or use.

## Verifying

```sh
cargo test                                   # unit + Python-mock tests
cargo test -- --ignored                      # rust-analyzer and pyright e2e
cargo clippy --all-targets -- -D warnings
```

The handle tests need a Python 3 that starts fast; `python3` behind a
version-manager shim can be too slow for the short backstops. They try a
few direct paths and honor `STRUCTURAL_TEST_PYTHON`.
