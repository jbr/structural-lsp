# Custom LSP extensions across mainstream servers, assessed for agent use

Companion to the harness survey. Where that one asked "what do harnesses call," this asks "what's actually on offer" — the non-standard methods, commands, and capability flags real language servers ship beyond the LSP spec.

**Sources and confidence.** rust-analyzer, clangd, and typescript-language-server sections are read directly from the projects' own extension docs in-repo (September 2026) and are reliable down to method names. gopls is from its command interface and MCP docs. Metals is from its integration guide plus its MCP feature docs. jdtls and the short-entry servers at the end are partially verified and marked as such. Ratings are my judgment, not anyone's claim.

**The organizing observation.** Server extensions fall into three piles, and only one of them matters:

1. **Editor ergonomics** — join lines, on enter, matching brace, move item, snippet edits, hover actions, code action groups, quick picks, status bars, decorations. These exist because a human is holding a cursor. An agent has no cursor. Dead on arrival, and they're the *majority* of most extension surfaces.
2. **Server internals** — memory usage, analyzer status, syntax tree dumps, HIR/MIR views, crate graph renders. Built for debugging the server. Mostly useless, with two exceptions noted below.
3. **Semantic surplus** — information the server computed anyway that the LSP spec has no request for. This is the whole prize, and it's a small fraction of each surface.

The good news is that pile 3 is consistently the most *structural* stuff — package-level queries, expansion results, dependency-of-a-selection, decompiled sources. Exactly the layer where an agent's view of code differs most from an editor's.

---

## rust-analyzer

The largest documented extension surface of any server. Full list from `docs/book/src/contributing/lsp-extensions.md`, with verdicts.

### Worth building against

| Extension | What it is | Verdict |
|---|---|---|
| **Hover Range** (`hoverRange` capability) | `textDocument/hover` accepts a `Range` where `Position` goes; returns the type of the selected *expression*. Their example: selecting `1 + 2 * 3` yields `i32`. | **High.** This is the closest thing in existence to the inlay-hint ask — on-demand type-at-arbitrary-expression, no whole-file cost. An agent that just wrote a long iterator chain can ask what it evaluates to. No standard LSP request does this; hover is otherwise pinned to a single position on a name. |
| **Expand Macro** (`rust-analyzer/expandMacro`) | Position → `{ name, expansion }`, the expanded text of the macro call. | **High.** Macro-generated code is invisible in source and is exactly what a model hallucinates about. `#[derive]`, `tokio::main`, `thiserror`, any proc macro. There is no non-LSP way to get this short of `cargo expand` (whole-crate, slow, requires nightly-ish tooling). Per-call-site expansion on demand is strictly better. |
| **Runnables** (`experimental/runnables`) | File (or position) → list of things that can be run, each with `kind: "cargo" \| "shell"` and fully-formed args: `cargoArgs`, `executableArgs`, `cwd`, `workspaceRoot`, env, optional `overrideCargo`. | **High.** This is the server handing you the exact command line, correctly scoped, instead of the model constructing `cargo test -p foo --lib -- some::path::test_name --exact` from memory and getting the separator wrong. Pure win for any harness that already has a bash tool. |
| **Related Tests** (`rust-analyzer/relatedTests`) | Position → the tests that exercise the symbol there, as `Runnable`s. | **High.** "Which tests cover the thing I just changed" without running the whole suite. Pairs directly with Runnables. Note this is heuristic in rust-analyzer, not a true coverage graph. |
| **Colored Diagnostic Output** (`colorDiagnosticOutput` capability) | Every flycheck `Diagnostic` carries `data.rendered`: the full compiler output **as cargo would print it**, optionally with ANSI. Present regardless of the capability; the flag only controls ANSI. | **High, and easy to miss.** LSP's `Diagnostic.message` is the one-line summary. `data.rendered` is the whole rustc block with the span underline, the `help:`, and the `note:` — which for Rust is often where the actual fix is. If you implement one thing from this document, read `data.rendered`. |
| **SSR** (`experimental/ssr`) | `foo($a, $b) ==>> ($a).foo($b)` style structural search-replace, scoped by document/position/selections, returns a `WorkspaceEdit`. `parseOnly` validates the query without computing edits. | **High.** Type-aware, path-resolving mechanical refactor as a single call, with a dry-run mode built in. Closest thing to Kiro's AST `pattern_rewrite` but semantic rather than syntactic. Caveat from their own doc: no search-without-replace mode, and no way to limit scope to a file set. |
| **Server Status** (`experimental/serverStatus`) | Notification with `health: "ok" \| "warning" \| "error"` and **`quiescent: boolean`** (is background work pending). | **High.** `quiescent` is exactly the readiness bit that distinguishes "no errors" from "not done indexing." The doc says clients are "discouraged from" using `health` to decide whether to send a request — that guidance is aimed at editors, where a stale answer is cheap. For an agent, acting on an incomplete answer is expensive, and gating on `quiescent` is the right call. |
| **Open External Documentation** (`experimental/externalDocs` + `localDocs`) | Position → `{ web?, local? }` doc URLs for the symbol. | **Medium-High.** Resolves the symbol to its docs.rs (or local `target/doc`) URL, correct for the actual resolved version. Composes with any rustdoc-side pipeline: this gives you the address, not the content. |
| **Controlling Flycheck** (`runFlycheck` / `clearFlycheck` / `cancelFlycheck`) | Notifications to trigger, clear, or cancel check runs. | **Medium-High.** Lets you run flycheck on demand rather than on save — which is the mechanism you want if you're building a check-when-I-ask loop rather than check-after-every-edit. `cancelFlycheck` matters if you're issuing edits faster than cargo can keep up. |
| **Workspace Symbols Filtering** (`workspaceSymbolScopeKindFiltering`) | Adds `searchScope: workspace \| workspaceAndDependencies` and `searchKind: onlyTypes \| allSymbols` to `workspace/symbol`. | **Medium-High.** Both axes cut result-set size hard, and `workspaceAndDependencies` is the one that lets you search *into* your dependencies — which is where an agent's API guessing actually happens. |
| **Parent Module** (`experimental/parentModule`) | Position → the module that contains this file. | **Medium.** Rust's module tree is not the file tree, and a model reading `src/foo/bar.rs` cannot reliably tell you its path without finding the `mod` declaration. Cheap to support, resolves a real ambiguity. |
| **Test Explorer** (`experimental/discoverTest`, `runTest`, `changeTestState`, `appendOutputToRunTest`, `abortRunTest`, `endRunTest`) | A full test discovery/execution protocol with streaming state changes and output. | **Medium.** Strictly more capable than Runnables — discovery without a position, structured pass/fail per test, streamed output. But it's a stateful multi-notification protocol, which is a lot of machinery relative to "shell out to the Runnables command line." Worth it only if you want structured test results rather than parsed stdout. |
| **Dependency Tree** (`rust-analyzer/fetchDependencyList`) | All crates in the workspace as `{name, version, path}`. | **Medium.** Resolved versions and on-disk source paths for every dependency, in one call. The `path` field is the useful part — it's where you'd point a doc or source reader. Note `cargo metadata` gives you a superset of this without the server. |
| **Interpret Function** (`rust-analyzer/interpretFunction`) | Evaluates the function under the cursor using rust-analyzer's own const-eval, without compiling. Labeled "highly experimental." | **Low-Medium, but interesting.** Compile-free evaluation is a genuinely agent-shaped idea — verify a pure function's behavior without a build. In practice the interpreter's coverage is narrow enough that I'd expect frequent failure. Worth a spike, not a dependency. |
| **Get Failed Obligations** (`rust-analyzer/getFailedObligations`) | Position → info about failed trait obligations. Explicitly labeled for debugging rust-analyzer itself. | **Low-Medium, speculatively.** Trait resolution failures are the single hardest Rust error class for a model to reason about, and the standard diagnostic often says "the trait bound X is not satisfied" without the obligation chain. If the output is legible this could be disproportionately valuable. It is not designed for this and may well be unreadable. Flagged as a gamble. |

### Not worth it

- **Join Lines, On Enter, Matching Brace, Move Item** — cursor ergonomics. `onEnter` in particular exists to continue doc comments when you press a key you don't have.
- **Snippet TextEdit, Code Action Groups, Hover Actions, Client Commands** — all about rendering affordances in a UI. `snippetTextEdit` adds tabstops to edits, which an agent must strip anyway.
- **View Syntax Tree / View HIR / View MIR / View ItemTree / View File Text** — the docs say outright these are "for debugging or when working on rust-analyzer itself." The output is rust-analyzer's internal dump format, not a stable structure. `viewSyntaxTree` is tempting as an AST source but you'd be better served by `syn` or tree-sitter.
- **View Crate Graph** — returns a rendered **SVG image**. Not a data structure.
- **View Recursive Memory Layout** — struct field offsets and sizes. Real information, near-zero agent applicability outside FFI and `repr` work.
- **Analyzer Status / Reload Workspace / Rebuild Proc Macros** — operational. `reloadWorkspace` is worth wiring to a "the Cargo.toml changed" trigger, but it's plumbing, not a capability.
- **Open Cargo.toml** — you can find Cargo.toml.

---

## clangd

Smaller surface, better hit rate. From `llvm/clangd-www/extensions.md`.

| Extension | What it is | Verdict |
|---|---|---|
| **Reference Container** (`textDocument.references.container` capability) | Adds `containerName` to each `Location` in a `textDocument/references` result — the function or class the reference occurs in. | **High, and the single best idea in this document.** A references result is normally a list of file:line pairs, which an agent has to *open every one of* to understand. Adding the enclosing symbol name makes the result self-describing at roughly zero token cost. Nothing in Rust or Go has an equivalent and it is the most obviously portable idea here. |
| **Inactive Regions** (`textDocument/inactiveRegions`) | Notification listing ranges disabled by preprocessor conditionals. | **High for C/C++.** Which half of an `#if` is live is invisible in the source text and an agent will confidently edit dead code. No other way to know without running the preprocessor. |
| **Inline fixes for diagnostics** (`codeActionsInline` capability) | Each `Diagnostic` carries its `codeActions` inline, rather than requiring a follow-up `textDocument/codeAction` round trip. | **High.** Halves the turn count on the fix-an-error loop, and more importantly it means the agent *sees* that a fix exists at the moment it sees the error. Pattern worth copying wherever the server will do it. |
| **Switch source/header** (`textDocument/switchSourceHeader`) | `.cpp` ↔ `.h`. | **Medium-High for C/C++.** Encodes a project convention the agent would otherwise guess at. Returns `""` when it can't determine one, which is at least an honest failure. |
| **Symbol info** (`textDocument/symbolInfo`) | Position → `{name, containerName, usr, id}` with no definition lookup and no index consult. | **Medium.** The `usr` (Unified Symbol Resolution) is a stable cross-tool symbol identity. If you want to key a cache, a memory, or a cross-session note on "this exact symbol," this is the identifier to use. Under-appreciated. |
| **AST** (`textDocument/ast`) | Range → the smallest enclosing `ASTNode` tree, with `role`, `kind`, `detail`, `arcana`, `range`, `children`. | **Medium.** Unlike rust-analyzer's syntax-tree dump this is a *structured*, documented response and it captures semantics (implicit casts, template instantiation) not just syntax. C++ is the language where "what does this expression actually do" is least visible in the text. Expensive in tokens; use on ranges, never files. |
| **UTF-8 offsets** (`offsetEncoding` negotiation) | Negotiate byte offsets instead of UTF-16 code units. | **High as plumbing.** LSP's UTF-16 position encoding is a persistent source of off-by-N on non-ASCII lines. Any server that lets you opt out of it, do. (LSP 3.17 standardized `positionEncoding`; clangd's predates it.) |
| **Code completion scores** | Completion items carry clangd's relevance score. | **Low-Medium.** Could rank a completion-as-API-discovery result. Marginal. |
| **File status** (`textDocument/clangd.fileStatus`) | Per-file worker thread activity. | **Medium as readiness signal**, same role as rust-analyzer's `quiescent`. Human-readable strings, so it's a heuristic, not a protocol. |
| **Memory usage**, **Force diagnostics generation**, **Diagnostic categories**, **Compilation commands** | Operational / minor. | **Low**, except `compilationDatabasePath` which you may need to configure at all. |

---

## gopls

gopls barely extends LSP. Its non-standard surface is `workspace/executeCommand` commands, and its own docs say they're implementations behind code lenses and code actions, not intended for direct client invocation, with an explicit "unpublish and remove this page" TODO.

Notable commands: `gopls.free_symbols`, `gopls.packages`, `gopls.modules`, `gopls.doc`, `gopls.list_known_packages`, `gopls.list_imports`, `gopls.add_import`, `gopls.change_signature`, `gopls.extract_to_new_file`, `gopls.add_test`, `gopls.run_tests`, `gopls.generate`, `gopls.tidy`, `gopls.assembly`, `gopls.gc_details`, `gopls.diagnose_files`, `gopls.run_govulncheck`, plus dependency and profiling commands.

The standouts:

- **`gopls.free_symbols`** — **High.** A query over a *selected range* reporting the set of symbols referenced inside it but defined outside it. That's "what does this block depend on," which is the question you need answered before extracting, moving, or summarizing code, and there is no LSP request for it. The single most agent-shaped idea in the gopls command set, and it generalizes to any language trivially.
- **`gopls.list_known_packages`** + **`gopls.add_import`** — **Medium-High.** "What could I import here" and "add it correctly." Import errors are a top-tier agent failure mode and this converts them from a guess into a lookup.
- **`gopls.gc_details`** / **`gopls.assembly`** — **Low in general, High for one niche.** Escape analysis and inlining decisions, and generated assembly. Irrelevant to most tasks; decisive for performance work, where a model otherwise reasons about allocation behavior from folklore.
- **`gopls.change_signature`**, **`gopls.extract_to_new_file`**, **`gopls.add_test`** — **Medium.** Semantic refactors as single calls. Same category as rust-analyzer's SSR: correct-by-construction edits.

### The bigger gopls story: it shipped an MCP server instead

As of gopls v0.20, gopls has a **built-in MCP server** (`gopls mcp`, or `-mcp.listen` for HTTP+SSE), with tools including `go_workspace`, `go_search`, `go_file_context`, `go_package_api`, `go_diagnostics`, `go_file_diagnostics`, `go_file_metadata`, `go_references`, `go_symbol_references`, `go_rename_symbol`, `go_vulncheck`, `go_context`. It ships model instructions describing read and edit workflows.

Three things about this are worth your attention:

1. **The tools are not LSP methods.** `go_file_context` returns "a summary of the declarations from other files in the same package that are used by the current file" — a compressed just-enough-context view with no LSP analogue at all. `go_package_api` is the package's surface, not a symbol list. `go_workspace` answers "is this a module, a workspace, or GOPATH." These are answers to *agent* questions, at a coarser granularity than LSP's per-position model.
2. **Attached vs detached mode** is exactly the document-sync problem from the harness survey, named and solved. Attached shares memory with a live LSP session and **sees unsaved buffer state**; detached runs headless and sees only disk. The Go team's answer to "which client owns the document" is to make it an explicit deployment mode.
3. **The instructions are prescriptive.** `go_file_context` "MUST be used immediately after reading any Go file for the first time." The server ships opinions about the agent loop, not just capabilities.

---

## TypeScript (typescript-language-server)

Commands under the `_typescript.` / `typescript.` namespace.

| Extension | Verdict |
|---|---|
| **`_typescript.goToSourceDefinition`** | **High.** Standard `textDocument/definition` on a library symbol lands you in a `.d.ts` declaration — a signature with no body. This walks through to the actual implementation source. For an agent trying to understand *behavior* rather than *shape*, the difference is the whole answer. (TS 4.7+.) |
| **`typescript.tsserverRequest`** | **High, with caveats.** A raw passthrough to the underlying tsserver protocol, with an `ExecuteInfo` object controlling execution target (semantic vs syntax server), async, and priority. tsserver's native protocol is substantially richer than its LSP projection — `navtree`, `quickinfo`, rename-file edits, and more. This is an escape hatch to all of it. Unstable and undocumented at the LSP layer, so it's a "when you need something specific" tool, not a foundation. |
| **`_typescript.organizeImports`** | **Medium-High.** Removes unused imports and sorts, also available as `source.organizeImports.ts` on save. Cleans up after exactly the mess agents make when iterating on a file. |
| **`_typescript.applyRenameFile`** | **Medium.** File rename with import updates — the TS analogue of rust-analyzer's `willRenameFiles` routing. |
| **`_typescript.applyRefactoring`**, **`_typescript.configurePlugin`** | **Low-Medium.** Plumbing. |
| **`$/typescriptVersion` notification** | **Low-Medium.** Which TS version is actually in play. Cheap, occasionally decisive for syntax-availability questions. |
| **Inlay hints** via `textDocument/inlayHint` with eight `includeInlay*` preference flags | **Notable for the inlay question.** TS gives you per-category control: parameter names, function parameter types, variable types, property declaration types, return types, enum member values. If a hint-annotated read is worth testing anywhere, this granularity is what makes it budgetable — you can turn on return types and enum values only, and skip the noisy ones. |
| **Code lenses** (references/implementations counts) | **Low.** The counts are mildly interesting as a cheap "is this widely used" signal, but the payload is an `editor.action.showReferences` command, i.e. UI. |

---

## Metals (Scala)

Metals' LSP extension surface is almost entirely client UI: `metals/inputBox`, `metals/quickPick`, `metals/status`, `metals/executeClientCommand`, `metals/didFocusTextDocument`, `metals/doctorVisibilityDidChange`, worksheet decorations. **Verdict on essentially all of it: None.** These exist so a server can drive an editor's chrome.

One structural detail is worth noting: library sources are materialized under `.metals/readonly` and `textDocument/definition` points there. The general pattern — **the server manufactures readable files for code that has no on-disk source** — recurs (jdtls's `java/classFileContents` does the same for decompiled class files). For an agent, that's the difference between "I can read this dependency" and "I can't."

### Metals also shipped an MCP server

Since v1.5.3, Metals serves MCP natively (`metals.startMcpServer`), with tools including `compile-file`, `compile-full`, `glob-search`, `typed-glob-search`, `inspect`, `get-docs`, `get-usages`, `find-dep`, `format-file`, `import-build`, `test`, `run-diagnostics`. Their stated motivation is blunt: agents invent functions, classes, and arguments that don't exist, so give them the server's real symbol knowledge.

Two tools here have no LSP equivalent and are worth stealing conceptually:

- **`inspect`** — for a package/object/trait, list its members; for a class, members plus constructors; for a method, **all overload signatures**. This is "show me the API surface of this thing," which is the completion-as-API-discovery idea from the harness survey, promoted to a first-class symbol-level query rather than a position-anchored one.
- **`typed-glob-search`** — symbol substring search filtered by symbol kind, result-capped via `-Dmetals.max-mcp-search-results` (default 100). Same shape as rust-analyzer's `searchKind` filter, with the cap the LSP version lacks.
- **`find-dep`** — resolve artifact names and latest versions. Small, and it targets a specific hallucination class.

---

## Shorter entries

**jdtls (Java)** — partially verified. Extensions in the `java/` namespace including `java/classFileContents` (**High**: source for compiled classes with no source on disk — the only way to read a dependency), `java/buildWorkspace`, `java/projectConfigurationUpdate`. Also an OSGi `delegateCommandHandler` extension point that lets third parties register arbitrary commands on the server — an unusually open-ended extension mechanism. Treat specific method names here as needing confirmation.

**pyright** — minimal by design. Believed to be roughly three commands (`pyright.createtypestub`, `pyright.organizeimports`, `pyright.restartserver`), none high-value. Not verified this pass. **Verdict: nothing to build against.** Pyright's value to an agent is entirely in standard LSP diagnostics and hover.

**Deno** — believed to have `deno/cache`, `deno/virtualTextDocument`, `deno/performance`, `deno/taskDefinitions`. Not verified. `virtualTextDocument` would be another instance of the manufactured-source pattern (reading remote-URL modules). Flagged for confirmation rather than asserted.

---

## Cross-cutting patterns worth stealing

Independent of any one server:

1. **Annotate results with their context.** clangd's `containerName` on references is the model. A result an agent can act on without a follow-up read is worth several that require one. Generalize: every location you return should carry the name of the thing it's inside.

2. **Attach the remedy to the problem.** clangd's inline `codeActions` and rust-analyzer's `data.rendered` are the same move — ship what the agent needs next alongside what it asked for, rather than making it discover that a next step exists.

3. **Ask about ranges, not positions.** `hoverRange` (type of an expression) and `gopls.free_symbols` (dependencies of a block) and `textDocument/ast` (structure of a selection) are all range-scoped, and all three answer questions that have no position-scoped form. LSP is position-shaped because cursors are; agents work in spans.

4. **Make the "not ready" state a protocol fact.** rust-analyzer's `quiescent`, clangd's `fileStatus`, Metals' status notification. This keeps coming up.

5. **Manufacture sources for what isn't on disk.** `java/classFileContents`, `.metals/readonly`, Deno's virtual documents. The agent's read tool should be able to open a dependency.

6. **Hand over the command line rather than describing it.** rust-analyzer's Runnables, with `cargoArgs` and `executableArgs` pre-split, is the server saying "here is the exact invocation" to a client that already has a shell.

7. **The two best-resourced server teams gave up on extending LSP and shipped MCP instead.** Go and Scala both concluded that the right agent interface was a *different, coarser* tool set — `go_file_context`, `go_package_api`, `inspect`, `compile-file` — not more LSP methods. Both ship model instructions with the server. That is a fairly strong signal about where the abstraction boundary actually wants to be, and it argues that the interesting design work for a harness is choosing what to synthesize on top of LSP rather than how faithfully to expose it.

---

## Shortlist for a Rust-first harness

If the question is what to actually implement, ranked by value over effort:

1. **Read `Diagnostic.data.rendered`.** Nearly free, and it's the difference between a one-line error and rustc's full explanation with spans and suggestions.
2. **`hoverRange`.** One capability flag, and it gives you type-of-expression on demand — the practical answer to the inlay-hints question.
3. **`expandMacro`.** Nothing else can tell the model what a derive or proc macro produced.
4. **`serverStatus` / `quiescent`** as the readiness gate.
5. **Runnables + relatedTests**, feeding your existing shell tool.
6. **`containerName`-style annotation on your own references results.** clangd won't give it to you for Rust, but you can compute it — resolve each reference location to its enclosing `documentSymbol` and label it. This is you building the extension rust-analyzer doesn't have, and it's probably the highest-leverage thing on this list after `data.rendered`.
7. **`workspaceAndDependencies` symbol search** for API discovery into crates.io deps.
8. **`externalDocs`** to resolve symbols to doc URLs, feeding whatever reads them.
9. **SSR**, if you want mechanical refactor. Dry-run via `parseOnly` first.
10. **A `free_symbols` equivalent.** Doesn't exist for Rust; would have to be built on `references` + scope analysis. Expensive, but it's the query that makes "extract this" and "explain this block's dependencies" tractable, and no one has it outside Go.

---

## Sources

- rust-analyzer: `docs/book/src/contributing/lsp-extensions.md` — https://github.com/rust-lang/rust-analyzer
- clangd: https://clangd.llvm.org/extensions (source: https://github.com/llvm/clangd-www)
- gopls commands: https://go.dev/gopls/ (`gopls/doc/commands.md` in golang/tools); command constants at https://pkg.go.dev/golang.org/x/tools/gopls/internal/protocol/command
- gopls MCP: https://go.dev/gopls/features/mcp · instructions: `gopls/internal/mcp/instructions.md`
- typescript-language-server: https://github.com/typescript-language-server/typescript-language-server (README)
- Metals LSP integration: `docs/integrations/new-editor.md` in scalameta/metals · MCP: https://scalameta.org/metals/docs/features/mcp/
- jdtls: https://github.com/eclipse-jdtls/eclipse.jdt.ls/wiki/Language-Server-Protocol-Extensions (partially verified)
