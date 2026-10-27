# Editor language-server inventories

Every language server that editor LSP clients ship built-in configuration
for, and how each client decides which server to start for a file and which
directory to use as the workspace root. Extracted from each editor's source
on 2026-09-15 into 2,218 normalized records, one per (source, server
definition).

The extracted records were the raw input to the catalogue's synthesis
([`../docs/catalogue.md`](../docs/catalogue.md)): which candidates the
conformance harness measures, the argv it starts them with, and the
consensus tiebreak. What the catalogue still reads is the per-executable
reduction, [`../tests/conformance/consensus.json`](../tests/conformance/consensus.json):
which inventories configure each of 760 executables. The records
themselves, copies of other projects' configuration, are not kept here.

## Sources

| Source | Revision | Extraction method |
|---|---|---|
| Eglot | `joaotavora/eglot` 52dddcb (2026-08-17) | `eglot-server-programs` read with the Emacs reader |
| lsp-mode | `emacs-lsp/lsp-mode` d083538 (2026-09-05) | every `make-lsp-client` / `make-lsp--client` form, read with the Emacs reader; variables, `lsp-dependency` and helper functions resolved |
| lsp-mode external clients | 18 repos from `lsp-client-packages` (ccls, lsp-biome, lsp-dart, lsp-grammarly, lsp-haskell, lsp-java, lsp-jedi, lsp-julia, lsp-latex, lsp-ltex, lsp-ltex-plus, lsp-metals, lsp-mssql, lsp-pascal, lsp-pyright, lsp-python-ms, lsp-sonarlint, lsp-sourcekit), HEAD on 2026-09-15 | same; `extra.package` names the repo. lsp-shader could not be cloned |
| lsp-bridge | `manateelazycat/lsp-bridge` 4459343 (2026-08-26) | `langserver/*.json`, `multiserver/*.json`; mode, extension and choice-variable tables read with the Emacs reader |
| nvim-lspconfig | `neovim/nvim-lspconfig` ac9d2f7 (2026-09-10) | each `lsp/*.lua` loaded in headless Neovim 0.12.5; computed fields kept as function source |
| Helix | `helix-editor/helix` master `languages.toml` (fetched 2026-09-15) | TOML |
| vim-lsp-settings | `mattn/vim-lsp-settings` c61a46e (2026-09-05) | `settings.json` plus each `settings/<server>.vim` (allowlist, blocklist, blocklist functions) and `checkers/` |
| kakoune-lsp | `kakoune-lsp/kakoune-lsp` 274740e (2026-09-12) | `rc/servers.kak` filetype hooks with TOML parsed, including commented-out alternatives |
| Kate | `KDE/kate` master `addons/lspclient/settings.json` (fetched 2026-09-15) | JSON; root precedence checked in `lspclientservermanager.cpp` |
| OpenCode | `sst/opencode` dev `packages/opencode/src/lsp/server.ts` (fetched 2026-09-15) | TypeScript compiler AST |
| Zed built-ins | `zed-industries/zed` f092e5e (2026-09-15) | `crates/languages` adapters (hand-encoded, checked by grep); `crates/grammars` language configs; `assets/settings/default.json` |
| Zed extensions | `zed-industries/extensions` fb1bef0 (2026-09-15), each extension at its pinned submodule commit | `extension.toml`, `languages/*/config.toml`, crate-wide scan of Rust source for `which()` and install calls |
| mason-registry (supplementary) | `mason-org/mason-registry` 9ad5f3b (2026-09-15) | `packages/*/package.yaml` (LSP category only). This is an installer index, not a selector |

Filetype → extension tables come from Neovim 0.12.5 `vim.filetype` internals, KDE `syntax-highlighting` 838dd85, Kakoune `rc/filetype` 1235028, the Emacs 29.3 `auto-mode-alist`, Zed language configs and OpenCode `language.ts`.

## Selection and root semantics by source

- **Eglot.** Selection: `eglot-alternatives` means the first entry found by `executable-find` wins; fixed contacts have a single command. Root: `project-current` (VC root by default) with `eglot-lsp-context` bound; otherwise `default-directory`.
- **lsp-mode.** Selection: filter clients by activation function or major mode, a binary `:test?`, and the enabled/disabled lists; then take the highest-`:priority` main client plus every `:add-on?` client. Root: session folders, then the projectile or project.el guess, or an interactive prompt.
- **lsp-bridge.** Selection: user hook, then extension table, then mode table. A multi-server config is used only if all of its commands exist; otherwise it falls back to the single server. Root: user function, then `.dir-locals.el`, then git toplevel, then single-file mode; `projectFiles` is searched upward (up to 20 levels) for servers that don't support single files.
- **nvim-lspconfig.** Selection: opt-in with `vim.lsp.enable`; everything enabled that matches the filetype and whose `cmd` is executable starts. Root: `root_markers` in priority order (nested lists mean equal priority), nearest ancestor; a `root_dir` function may decline to return a root.
- **Helix.** Selection: all servers in the language's list start; each feature goes to the first capable server in list order. Root: the topmost marker between the file and the workspace (`.git`/`.svn`/`.jj`/`.helix`). Markers are per language, not per server; `required-root-patterns` gates each server.
- **vim-lsp-settings.** Selection: the first available entry in filetype order is registered and the loop stops, unless `g:lsp_settings_filetype_<ft>` is a list. Root: nearest `root_uri_patterns` match or `.git`/`.svn`/`.hg`/`.bzr`.
- **kakoune-lsp.** Selection: all active entries in the filetype hook run. Root: `root_globs` tried in list order, each searched all the way to `/`, so the first glob found anywhere above wins.
- **Kate.** Selection: one server per entry, matched by `highlightingModeRegex`. Root: explicit `root` setting, else nearest `rootIndicationFileNames`/`Patterns`.
- **OpenCode.** Selection: every server whose extension matches and whose `root()` returns a path starts. Root: nearest include marker, bounded by the project directory; any exclude marker upward means the server is skipped.
- **Zed.** Selection: the `language_servers` setting, where `...` expands to all remaining servers and `!x` excludes one. Adapters check a configured binary path, then `which`, then download. Root: the worktree; the Cargo.toml and pyproject.toml manifest providers refine it.

## Confidence and known gaps

- **Exact:** Helix, nvim-lspconfig static commands, lsp-bridge, Kate, kakoune-lsp, vim-lsp-settings command names, Eglot fixed commands and alternatives, OpenCode `which()` arguments.
- **Heuristic:**
  - lsp-mode executables are resolved from variables, `lsp-dependency :system/:path` (including simple `concat`), and `executable-find` in helper functions. 26 clients remained unresolved, with only the source expression recorded. `pyright-remote` shows `pyright`, the prefix of a runtime concat.
  - nvim-lspconfig's 26 computed commands use string literals from the function body.
  - Zed extension `which()` calls are scanned across the whole crate, so extensions with several servers attribute all binaries to each (a note marks these). 145 Zed extension servers have no resolved executable, mostly because the extension never calls `which()` and always downloads the server.
- **Extensions for Emacs sources are weakest.** The Emacs 29.3 `auto-mode-alist` doesn't include third-party modes (rust-mode, go-mode, etc.), so many modes are mapped by name (`foo-ts-mode` → `foo`) to another editor's table. lsp-mode clients that use a custom `:activation-fn` (e.g. `ts-ls`) have no derived extensions.
- **Vim filetypes** are mapped using Neovim's detection tables as a proxy for Vim's `filetype.vim`. Content-dependent entries resolve to Neovim's guess for an empty file.
- **Zed language names:** 64 names that extensions list for their servers aren't defined by any fetched language config (e.g. `Cpp`, `Vue`, `Yaml`). These servers can't activate under those names.
- **Not covered:** VS Code extensions, Sublime LSP, coc.nvim, JetBrains, lsp-shader, and runtime user configuration in general.

## Upstream quirks found during extraction

- **Eglot:** the TypeScript alternatives list the program as the single string `"rass ts"`, which `executable-find` will never resolve.
- **Kate:** the markdown entry uses `rootIndicatorFileNames`, but the code reads `rootIndicationFileNames`, so its root markers are ignored.
- **Helix:** `haxe-language-server` is referenced by a language but not defined; 21 defined servers aren't attached to any language.
- **lsp-bridge:** some server files (e.g. `deno`, `typst-lsp`, `ds-pinyin`) aren't reachable from any default table or documented choice variable.
- **vim-lsp-settings:** the filetype loop stops once it registers the first available server, while blocklists are evaluated later per buffer. For TypeScript this means `deno` is never registered while `typescript-language-server` is installed, unless both are listed in `g:lsp_settings_filetype_typescript`.
