# reference

External findings and inputs: what other people's software does, what a
dataset says, what the project looked like before it existed. Nothing here
describes structural's current behaviour — that is [`../docs/`](../docs/) —
and nothing here is a work item — that is [`../backlog/`](../backlog/).

A document belongs here when it would still be true if this repository were
deleted. It is read when a decision needs evidence, not every session.

- [`harness-survey.md`](harness-survey.md) — how existing agentic coding
  harnesses wire language servers into an agent loop (September 2026). The
  survey behind the two-surface framing and the decision to be an MCP server
  rather than an in-harness integration.
- [`lsp-server-extensions.md`](lsp-server-extensions.md) — the non-standard
  methods and commands real language servers ship beyond the LSP spec, rated
  for agent use. The source of `expanded`, `related-tests` and `hoverRange`
  (`view = "type"`).
- [`server-root-discovery.md`](server-root-discovery.md) — whether a language
  server finds its own project root when started inside one. rust-analyzer
  and gopls both walk up and index the whole enclosing project, measured
  September 2026; the client's filter, not the server, is what narrows an
  answer. The evidence behind the root rule in
  [`../backlog/discovery/selection.md`](../backlog/discovery/selection.md).
- [`servers-list/`](servers-list/) — thirteen editor inventories (Eglot,
  lsp-mode, lsp-bridge, nvim-lspconfig, Helix, vim-lsp-settings,
  kakoune-lsp, Kate, OpenCode, Zed, mason-registry) extracted from source and
  merged into 2,218 normalized records, with its own confidence tiers in its
  README. It is the raw input to a server catalogue, not a catalogue: the
  synthesis — which servers this project stands behind, in what order, with
  which root rules — has not been done.
- [`design-draft.md`](design-draft.md) — the original design draft
  (2026-09-13), written before anything was implemented. Kept because it
  states the premises the tools are still built on and the open questions
  with the cheapest experiment for each. Where it disagrees with
  [`../docs/`](../docs/), the docs are current and this is history.
