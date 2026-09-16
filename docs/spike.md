# The raw-LSP spike

`src/bin/spike/` is a separate binary with **no MCP surface**: it starts one
or two language servers, waits for readiness, opens the annotated fixture
files, and dumps the raw results of `workspace/symbol`,
`textDocument/definition`, `textDocument/references` and
`textDocument/codeAction` for every `@decl` and `@expr` annotation in
`fixtures/`.

It exists to see what a server actually returns, unrendered, when a tool's
answer is surprising — the question "is this our rendering or their result?"
is answered here without adding logging to the tools.

Read-only by construction: it never applies an edit, never executes a
code-action command, and answers `workspace/applyEdit` with
`{"applied": false}`.

```sh
cargo run --bin spike                       # both servers, default bounds
cargo run --bin spike -- --server rust --quiesce 120 --out target/spike
cargo run --bin spike -- --server python --pyright-wait 6 --query-timeout 30
```

Output is deterministic (default `target/spike/<server>/`) so two runs are
comparable: `raw/` holds one JSON file per query, and `manifest.json` holds
the initialize result, readiness evidence, timings, and every notification
and server request seen.

`cargo test` covers its annotation resolver; the transport tests it shares
with the rest of the crate do not need a language server.
