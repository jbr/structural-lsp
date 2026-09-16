# LSP integration in agentic coding harnesses

A survey of how existing harnesses actually wire language servers into an agent loop, and the design decisions that fall out of it. Written as input for someone about to implement this.

**Method and confidence.** Everything below comes from vendor documentation, source READMEs, or issue trackers, gathered September 2026. Where a claim comes from a third-party write-up rather than the project's own docs, it's marked. Vendor self-description is marked too — a few projects make comparative claims about competitors that aren't independently verified, and those are reported as claims, not facts. Operation lists are as documented; they drift fast.

---

## 1. There are two integration surfaces, and they're orthogonal

Every implementation picks one or both:

**(a) Ambient — diagnostics folded into the results of tools the agent already calls.** The agent never asks for LSP anything. It edits a file, and the edit result comes back carrying whatever the language server said about that file. No new tool, no new decision for the model to make.

**(b) Explicit — a navigation tool the model calls deliberately.** Usually one tool with an operation enum, occasionally N separate tools. The model decides when it wants a definition or a reference set.

These solve different problems and fail differently. (a) has zero prompting cost and zero chance the model forgets to use it, but it can only tell the agent about the file it just touched. (b) gives real navigation power but competes for the model's attention against grep, which is familiar and usually good enough — the recurring complaint in every project that ships (b) is that models under-use it.

The distribution across the field:

| Harness | Ambient | Explicit | Notes |
|---|---|---|---|
| Hermes Agent | ✅ | ❌ | Diagnostics only, no navigation at all |
| OpenCode | ✅ | experimental | Explicit tool is env-var gated |
| Claude Code | ✅ | ✅ | Both, plugin-gated |
| Kiro | ✅ | ✅ | Plus a non-LSP tree-sitter layer |
| OMP | ✅ | ✅ | Widest op surface of the CLI harnesses |
| Kilo Code | ✅ | ✅ | Per third-party description |

---

## 2. Per-implementation findings

### OMP (oh-my-pi)
Fork of Mario Zechner's Pi, TypeScript shell over a Rust core. Advertises **14 LSP ops exposed to the model as first-class tools**: `diagnostics`, `definition`, `type_definition`, `implementation`, `references`, `hover`, `symbols`, `rename`, `code_actions`, `status`, `reload`, `format`, `completion`, `signature_help`. (That list is from a third-party deep-dive; a fork's README lists an 11-op subset without `format`/`completion`/`signature_help`, probably an older snapshot.)

Ambient behavior: diagnostics checked after every edit; format-on-write goes through the language server's formatter rather than a separate tool call. Rename is routed through `workspace/willRenameFiles` so re-exports, barrel files, and aliased imports update before the file moves. Local binary resolution checks `node_modules/.bin/` and `.venv/bin/` before PATH.

Notable: `status` and `reload` are in the op enum. Server lifecycle is a thing the *model* can inspect and act on, not just the operator.

### Kiro (AWS)
Two layers under a single `code` tool. Tree-sitter is always on and needs no setup: `search_symbols` (fuzzy), `lookup_symbols`, `get_document_symbols`, `pattern_search` (AST structural search with `$VAR` / `$$$` metavariables), and AST `pattern_rewrite`. LSP is opt-in per workspace via `/code init`, writing `.kiro/settings/lsp.json`, and adds find-references, go-to-definition, rename, diagnostics, and hover.

Two details worth stealing:
- **Rename supports a dry run.** "Dry run: would rename 12 occurrences in 5 files" before anything is applied.
- **Completion is framed as API discovery.** Their own docs demo it as "what methods are available on the `s3Client` instance?", answered with a list of names and full type signatures. Not autocomplete — a typed, receiver-aware documentation lookup.

Their documented failure mode for go-to-definition is telling: "Position doesn't point to a symbol → verify the row and column numbers point to a symbol name." That's the addressing problem (§4.1) showing up in a troubleshooting table.

### OpenCode
Ambient layer: LSP is **disabled by default**; when enabled, servers start on file open by extension match, and diagnostics feed the agent. ~35 built-in server configs with auto-install for many.

Explicit layer: an `lsp` tool gated behind `OPENCODE_EXPERIMENTAL_LSP_TOOL=true`, with ops `goToDefinition`, `findReferences`, `hover`, `documentSymbol`, `workspaceSymbol`, `goToImplementation`, `prepareCallHierarchy`, `incomingCalls`, `outgoingCalls`. Pure navigation — no rename, no code actions, no completion, nothing that mutates.

**Their docs argue against their own feature**, which is the most useful data point in the whole survey: language servers get out of sync, use significant memory, vary by version and project, and slow down agent workflows; in many projects it's better to have the agent run lint/typecheck CLI tools directly and document those commands in `AGENTS.md`. Also relevant: OpenCode V2 has no LSP runtime at all yet — the config is accepted and preserved but starts no servers and exposes no tool.

### Hermes Agent (Nous Research)
No navigation surface. LSP is **exclusively** a post-write check on `write_file` and `patch`, and the mechanism is the most carefully engineered part of anything surveyed:

1. Capture a baseline of current diagnostics for the file.
2. Perform the write.
3. Re-query, filter out everything already in the baseline, surface only the new ones.

Plus:
- **Freshness gating.** A diagnostic only counts if the server produced it for the post-edit content. A slow server yields "no LSP data" for that edit rather than yesterday's errors. `wait_timeout` defaults to 5s.
- **Two independent channels.** `lint` carries an in-process syntax check (microsecond `ast.parse` / `json.loads`); `lsp_diagnostics` carries the semantic result. A syntax-clean file with type errors reports `lint: ok` plus a populated `lsp_diagnostics`.
- **Layered fallback.** Syntax check first, LSP second only if syntax is clean. Every LSP failure path degrades silently to the syntax-only result — a flaky server can never break a write.
- **Gated on git workspace detection.** No repo, no LSP. Avoids spawning pyright because someone edited a file in `$HOME`.
- Lazy spawn, idle reaping at 600s default (clamped to ≥30s so a sweep can't reap mid-operation), crashed servers go into a broken-set and aren't retried for the session, multi-root attach for pyright so sibling worktrees share one process.

There's an open issue proposing a Kilo-Code-style navigation tool. Not implemented.

### Claude Code
Per the official tools reference: a builtin `LSP` tool that **automatically reports type errors and warnings after each file edit**, plus direct calls for definition, references, type info at a position, list symbols in a file, workspace symbol search, find implementations, and call hierarchies. No rename, no code actions, no completion.

Configuration lives in plugins (`.lsp.json` at the plugin root); the tool stays inactive until a code-intelligence plugin is installed, and you install the server binary yourself. It was env-gated behind `ENABLE_LSP_TOOL=1` before v2.0.74.

One design detail worth noting: **LSP shares the `Read(path)` permission-rule format** with Read/Grep/Glob. Semantic queries are classified as reads for authorization purposes, which is the obviously correct call and something worth getting right up front rather than retrofitting.

### Kilo Code
Reported (via the Hermes issue that proposes copying it, not from Kilo's own docs): post-edit diagnostics in the tool result, **including diagnostics from other files affected by the change**, capped by a `MAX_PROJECT_DIAGNOSTICS_FILES` limit around 5. Plus a dedicated LSP tool for go-to-definition, find-references, hover, and call hierarchy.

The cross-file diagnostic propagation is the interesting bit and nobody else surveyed does it explicitly. If you change a function signature, the errors appear in the callers, not in the file you edited.

### Docker's agent
LSP servers are configured as **"toolsets"** managed by the same supervisor as MCP toolsets, with a per-toolset `lifecycle` block (`resilient` = auto-restart with exponential backoff; `strict` for CI). It exposes a per-server **capability introspection listing** (Hover: yes, Rename: yes, Type Hierarchy: yes, …) and documents that **all positions are 1-based**, not LSP's native 0-based.

Both of those are small and both matter. Capability introspection means the model can be told what this particular server actually supports rather than discovering it through failures. 1-based positions match what every `read` tool already shows the model.

---

## 3. The MCP layer (where the more interesting design work is happening)

These aren't harnesses, but several harnesses get their LSP capability from them, and the design space explored here is wider.

### Serena (oraios, MIT, ~25k stars)
The de facto standard. Its distinguishing move is **symbol-level editing, not just navigation**: `find_symbol`, `get_symbols_overview`, `find_referencing_symbols`, `rename_symbol`, `replace_symbol_body`, `insert_before_symbol`, `insert_after_symbol`. The pitch is explicitly against line numbers and primitive search patterns as the addressing mechanism for edits — you say "replace the body of `UserRepository.find`", not "replace lines 40–58" or "replace this exact string".

`get_symbols_overview` is worth calling out separately: a symbol map of a file or directory **without reading the source**. That's a read-tool replacement, not an LSP query, and it's a direct attack on context cost.

### agent-lsp (blackwell-systems, MIT, Go)
Vendor self-description, so treat the comparative claims with salt, but the ideas are the most developed in this space:

- **Speculative execution (8 tools).** `preview_edit` / `simulate_edit` apply a change to the server's in-memory document, read the diagnostic delta, then apply or discard without touching disk. `simulate_chain` evaluates a dependent sequence of edits and reports which step first introduces an error. `safe_apply_edit` previews then applies to disk only if `net_delta == 0`.
- **Batching.** `blast_radius` returns all exports plus all callers in one call, partitioned test vs non-test — claimed to replace 20+ sequential LSP calls.
- **Auto-diagnostics on symbol edits.** `replace_symbol_body`, `insert_after_symbol`, etc. return `errors_after` / `warnings_after` counts inline, so there's no separate diagnostics call.
- **Phase enforcement.** When a skill is active, tool calls are checked against the current phase; calling `apply_edit` during the analysis phase returns an error naming the allowed tools instead of proceeding. Runtime enforcement of workflow ordering rather than trusting the prompt.
- **Persistent warm daemon** surviving between agent sessions, auto-exiting after 30 min idle, with fast-indexing languages bypassing it entirely.
- **Non-JSON output encoding** for token reduction, with claimed 30–84% savings depending on response shape.

### Others in the same shape
`mcp-language-server` (Go) exposes six tools: definition, references, diagnostics, hover, rename_symbol, edit_file. A "Codex LSP Bridge" exposes go-to-def, type definition, references, rename, hover, signature help, document/workspace symbols, code actions, format, call hierarchy, diagnostics, **`wait_for_diagnostics`**, a status probe, and `resolve_symbol` (fuzzy name-based lookup). The `wait_for_diagnostics` and `resolve_symbol` pair is the same two problems everyone hits: when are results ready, and how do you address a symbol you can't point at.

### Academic: OPENDEV (arXiv 2603.05344)
Six LSP tools split into two read-only navigation tools and four structural editing tools, behind a **four-layer abstraction**:

1. *Agent Tool Layer* — each tool takes a file path and a **symbol name**, never a position. Language detection, server selection, and protocol translation are invisible to the model.
2. *Symbol Retriever* — resolves names to positions via a `NamePathMatcher` supporting exact (`MyClass.method`), partial-path (`method` matches anything ending in `.method`), and wildcard (`My*`) matching.
3. *LSP Server Wrapper* — language detection across 30+ extensions, singleton pool with one server per language, lazy start, liveness checks, transparent restart on crash.
4. *Language Server* — JSON-RPC over stdio, two daemon threads per server for I/O, thread-safe request IDs, configurable per-request timeouts.

This is the cleanest articulation of the addressing argument: the agent should never compute a line and column.

---

## 4. What converges, and the decisions that remain

### Universal
Definition, references, hover, document symbols, workspace symbols, diagnostics. If you ship nothing else, ship these.

### Common but not universal
Type definition, implementation, call hierarchy (incoming/outgoing), rename, code actions, format, server status/reload.

### Shipped by almost nobody
Completion, signature help (OMP and Kiro only). Type hierarchy (MCP servers only).

### Shipped by literally nobody found
**Inlay hints. Semantic tokens. Code lens. Document highlight. Folding ranges. Linked editing.** Every visual affordance is absent from every harness surveyed. That's absence of evidence rather than evidence against, but it's consistent absence.

---

## 4.1 Decision: how are positions addressed?

This is the single biggest design fork and it's under-discussed.

- **Position-based** (line, column): Claude Code, OpenCode, Kiro, OMP, Docker. Matches LSP directly, zero translation cost. But the model has to get a column right, which is a known failure mode — Kiro documents "verify the row and column numbers point to a symbol name" as a troubleshooting entry, and both the Codex bridge (`resolve_symbol`) and agent-lsp (`/lsp-edit-symbol`: "edit a named symbol without knowing its file or position") ship explicit workarounds for it.
- **Name-based**: Serena, OPENDEV, and the workaround tools above. The harness resolves a name to a position internally. Costs you a resolver with an ambiguity policy; buys you tool calls that don't depend on the model's arithmetic over a file it saw N turns ago.

Cheapest sound answer: accept both. Name-based as the primary interface, position-based as the escape hatch, with 1-based positions matching whatever your read tool displays.

## 4.2 Decision: what is the diagnostics loop actually for?

"Compile after every edit" is not the valuable thing, and the implementations that do it well aren't doing that.

The valuable thing is **attribution**. `cargo check` in a shell hands the model the entire error set including the forty that predate the edit, and makes it work out which ones are its fault. Hermes's baseline-diff hands it only the delta. Kilo Code's version additionally follows the delta into *other* files. Neither is obtainable by shelling out to a build tool.

The second valuable thing, which only the speculative-execution tools do, is **moving the check before the write**. If you can `didChange` an in-memory document, read the diagnostic delta, and discard, then the diagnostics loop stops being "notice you broke something" and becomes "decline to break it." That reframing answers the objection to post-edit compilation directly: the compile isn't intrinsically valuable, but a cheap counterfactual is.

Costs to plan for either way: freshness (never report a result the server produced before the edit — report nothing instead), a wait budget, and a silent degradation path so a crashed server can't fail a write.

## 4.3 Decision: what does the model get told about server state?

Everyone eventually exposed this, in some form: OMP's `status`/`reload` ops, Kiro's `/code status` and `/code logs`, Hermes's `hermes lsp status` and broken-set, agent-lsp's `doctor`, Docker's `lifecycle` profiles and capability listing.

"No diagnostics" and "server still indexing" are different facts, and a model that can't distinguish them will confidently report that a file is clean. For rust-analyzer this is the whole ballgame: large codebases can be slow to index initially, cold start runs 10+ seconds on a fresh project, and the first edit after server start commonly returns nothing with subsequent edits picking things up. Plan for a tri-state (`clean` / `errors` / `not ready`) rather than a diagnostic list that's empty for two different reasons.

Capability introspection belongs here too. Not every server implements every op — Kiro notes some don't support rename or formatting. Telling the model up front beats a failed call.

## 4.4 Decision: one tool with an op enum, or N tools?

The CLI harnesses almost all chose **one tool with an operation enum** (Kiro's `code`, OMP's `lsp`, OpenCode's `lsp`, Claude Code's `LSP`). The MCP servers chose **N tools**, and have drifted to 6, 14, 50, 65 of them.

The op-enum shape keeps one tool definition in the prompt and one permission rule. The N-tools shape gets better tool-selection behavior from the model, at a context cost that grows linearly and eventually needs a tool-search mechanism to manage. For an in-process harness where you control the prompt, the enum looks clearly right.

## 4.5 Decision: is LSP a read tool or an edit tool?

The split is roughly: CLI harnesses use LSP for navigation and occasionally rename; MCP servers (Serena, agent-lsp, OPENDEV) use it as the *primary editing substrate* — replace this symbol's body, insert after this symbol, delete this symbol safely.

Symbol-level editing sidesteps the whole class of string-match edit failures (whitespace drift, non-unique `old_string`, stale line numbers) by addressing the AST instead of the text. It's a much bigger commitment than adding a navigation tool, and it competes with whatever edit primitive you already have. Worth knowing it's the direction the more ambitious projects went.

## 4.6 On completion, signature help, and inlay hints

The premise that these have no agentic analogue is mostly right in practice and probably wrong in principle.

**Completion** does have one, and Kiro's framing is the right one: it's not autocomplete, it's *"enumerate the API surface of this value, with types."* An agent's failure mode is guessing at APIs. `textDocument/completion` at a receiver position answers "what can I call on this" from the actual resolved dependency version, not from a plausible-looking memory of the library. Same argument for `signature_help` at a call site. Two of the more aggressive harnesses ship both; the rest don't, and the omission looks like inattention rather than a considered judgment.

**Inlay hints** are unimplemented everywhere found. The likely reason is token cost: whole-file inlays are a lot of output for information the model usually infers correctly. But the cost/benefit is language-dependent, and Rust is the best case anywhere — elided `let` bindings, closure parameters, and long iterator chains carry real inferred-type information that a TypeScript file mostly doesn't. A `read` variant that interleaves type hints, budgeted and scoped to a range rather than a file, is an unexplored idea rather than a refuted one. It is also the only one on this list that requires building something nobody has a reference implementation for.

---

## 5. Rust-specific notes

- rust-analyzer's **own** diagnostics are thin; most real errors arrive via flycheck (`cargo check`). A design that disables check-on-save gets navigation for free but loses most of the diagnostic value, and needs its own `cargo check` path to compensate.
- Cold start and indexing dominate. Every freshness/readiness mechanism above exists mostly because of servers like this one.
- OpenCode requires `rust-analyzer` to already be on PATH (no auto-install); Kiro and Hermes both list it as manual install via rustup. Nobody auto-provisions it.

---

## 6. Things worth testing before committing

1. How often does a model actually call an explicit navigation tool when grep is also available? The recurring complaint suggests: not often. Measure it before building 14 ops.
2. Does baseline-diff attribution measurably beat just running `cargo check` and letting the model sort it out?
3. What's the real token cost of a references result on a large symbol, and does it need capping? (agent-lsp caps at 100 with a "narrow your query" line; the Praison tools do the same.)
4. Does a speculative `didChange` → diagnostics → revert cycle actually work reliably against rust-analyzer, or does it fall over on flycheck's disk dependency? This is the highest-value and highest-risk item on the list.
5. Is a type-hint-annotated read cheaper or more expensive, in tokens, than the follow-up hover calls it would replace?

---

## Sources

- OpenCode: https://opencode.ai/docs/lsp/ · https://opencode.ai/docs/tools/ · https://opencode.ai/v2/docs/lsp/
- Kiro: https://kiro.dev/docs/tools/code-intelligence/ · https://kiro.dev/docs/reference/built-in-tools/
- Hermes Agent: https://hermes-agent.nousresearch.com/docs/user-guide/features/lsp · issue #516 (Kilo Code comparison): https://github.com/NousResearch/hermes-agent/issues/516
- Claude Code: https://code.claude.com/docs/en/tools-reference.md (LSP tool behavior)
- OMP: https://github.com/can1357/oh-my-pi · op list via https://acchapm1.github.io/tutorials/Oh-My-Pi/omp-deep-dive (third-party)
- Docker agent LSP toolsets: https://docs.docker.com/ai/docker-agent/tools/lsp.md
- Serena: https://github.com/oraios/serena
- agent-lsp: https://github.com/blackwell-systems/agent-lsp (vendor description)
- OPENDEV: https://arxiv.org/pdf/2603.05344 §2.4.5
