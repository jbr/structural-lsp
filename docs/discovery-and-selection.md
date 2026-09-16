# Discovery and selection: the plan for unit 3

Parent: `docs/lifecycle-and-discovery.md`, unit 3 — "Add discovery and
selection as pure policy". Predecessors: `docs/host-extraction.md` (unit 1),
`docs/server-lifecycle.md` (unit 2).

## Done when

Quoted from the parent plan:

- fixture trees produce deterministic plans;
- irrelevant servers do not start;
- overrides win;
- an unavailable preferred candidate can fall back to an available
  alternative;
- selection and project-root choices are explainable.

Plus, from the unit's own text: selection is **independently testable without
spawning**, and acquisition is connected to inventory and eager warming only
after that.

**Not included:** filesystem observation (unit 4), the synchronization
contract (unit 5), the per-call `workspace` argument (unit 6), automatic
installation, and any merging of overlapping semantic roots.

## Where unit 2 left it

Facts from the code:

- **Selection is today's configuration, eagerly.** `Workspace::new` lays out
  one slot per `Config::adapters()`: the two built-ins, plus whatever
  `structural.toml` names — built-ins overridden in place, other entries
  generic. **Every slot is warmed at `agent_started`, whether or not anything
  in the tree could use it.** An unrelated Rust server starts in a Python-only
  project today.
- **Routing is by extension.** `language_for(languages, path)` matches a
  file's extension against an adapter's languages, and `workspace::serving`
  turns that into the slots a scope needs. There is no notion of evidence,
  applicability, or preference: a configured server is a candidate for exactly
  the extensions it declares.
- **The extension table is fixed.** `default_extensions` in `config.rs` maps
  eleven language ids to extensions; anything else must spell them out in
  `structural.toml`. This is a catalogue in embryo, living in the configuration
  parser, and it is what `docs/servers-list/` (1,106 extensions across thirteen
  editors) replaces with data.
- **A built-in cannot be disabled.** `adapters()` always emits both built-ins;
  an entry with a built-in's name can only override its command, languages or
  backstop. There is no way to say "not in this project".
- **Availability is not modelled.** A server that will not start is `Failed`
  on its slot (unit 2) and reported per call. Nothing tries a second
  implementation of the same language.
- **Roots do not exist as a concept.** A workspace is one canonical root; a
  tool's `path` is a scope inside it. `Workspace::stop` and `select` never ask
  what project a file belongs to.

## Shape

Four layers, each testable alone, in the parent doc's terms:

```text
evidence      what the tree says it is      (manifests, sources, markers)
   |
catalogue     what a server is, and what it can serve   (declarative entries)
   |
selection     evidence + catalogue + overrides -> plans (pure; no spawning)
   |
acquisition   start the planned servers                 (unit 2's slots)
```

The seam that matters is **selection is a function, not a side effect**. It
takes evidence, the catalogue and the overrides, and returns *plans* — each
naming a server, its applicable scope, and the reasons it was chosen — and it
does nothing else. No processes, no filesystem watching, no locks. That is
what makes the Done-when clauses testable: a fixture tree plus a catalogue
yields a value, and the value is asserted.

## What evidence is

Evidence is what a tree says about itself, gathered by reading it:

- **sources** — extensions present, by count and by directory;
- **manifests** — `Cargo.toml`, `pyproject.toml`/`setup.py`, `go.mod`,
  `package.json`, `tsconfig.json`, `build.gradle`, and the like;
- **markers** — lockfiles, toolchain files, editor or CI configuration that
  says something about the toolchain.

Evidence is **evidence, not proof**, and the parent doc is explicit that a
marker influences preference only where the relationship is verified. A
`package.json` does not by itself prove that a JavaScript server applies, nor
that any particular command exists. So evidence yields *candidates and
reasons*, and the catalogue decides what to do about them.

Candidate project roots are evidence's other output: a `Cargo.toml` below the
workspace root is a candidate root, not a fact. Which candidates are adopted is
selection's decision, and the parent doc keeps three concepts apart on purpose —
the workspace an agent selected, the project roots discovered inside it, and
the filesystem coverage observation will need. **Discovering `/repo/subproject`
never merges it with `/repo`.**

## What the catalogue is

A catalogue entry is a server: how to launch it, what it serves, when it
applies, and how it compares to alternatives. Most entries use the generic LSP
path, and a broad catalogue does **not** mean an adapter per server —
`Adapter` is only for protocol behaviour that differs.

An entry needs to express, at minimum:

- **identity and command** — name, argv, install hint;
- **languages** — language ids and extensions, which is what `default_extensions`
  is today, moved out of the config parser and into data;
- **applicability** — what evidence makes it a candidate, if anything beyond a
  language being present;
- **preference** — an ordering among candidates for the same language, and
  *why*, so a choice can be explained;
- **alternatives** — other entries that can stand in when this one is
  unavailable;
- **complementarity** — entries that are *additions* rather than alternatives:
  a linter or formatter serving the same files alongside a full language
  server. `language -> executable` cannot express this, which is why the parent
  doc rejects it.

The catalogue is **data, not code**: table-driven, so adding a server is a
declarative entry rather than a build. What is *in* it starts small and
verified — entries whose command and applicability we have actually checked —
and broadening it is ordinary follow-up work, not this unit's.

## What selection returns

A selection is a list of plans, and every plan carries its reasons:

```text
plan:
  server        catalogue entry chosen
  scope         the files or directories it applies to
  root          the project root it would be started against
  why           the evidence and overrides that put it here, and what it beat
```

Three properties the parent doc asks for, and they are all properties of this
value rather than of anything running:

- **deterministic** — the same tree and catalogue give the same plans, in the
  same order;
- **explainable** — every plan says why, including why an alternative was not
  chosen;
- **honest about availability** — a plan is a *proposal*; whether the server
  can actually start is settled by acquisition, and a failure is reported
  rather than silently dropped.

## Overrides

`structural.toml` keeps working and keeps winning. Today it *is* the
configuration; after this unit it is the override layer over a catalogue:

- naming a server configures it — command, languages, backstop;
- disabling a server becomes possible, which it is not today, because a
  project that has no Rust in it should be able to say so;
- adding a server the catalogue does not know stays possible, generically.

Precedence is explicit and ordered: an override beats the catalogue, and the
catalogue beats a guess from evidence. The parent doc notes exact syntax and
default precedence are unsettled; this plan fixes the *order* and leaves the
spelling of it to implementation.

## Availability and fallback

The parent doc is careful here and so is this plan: availability means
**successful initialization, not an executable on `PATH`**. So selection
proposes, acquisition attempts, and the outcome flows back:

- an attempt that fails records the failure against its plan;
- a failed candidate may lead to the next alternative for that language;
- this is **bounded selection**, not an indefinite restart loop: candidates are
  tried in preference order and the list ends;
- the failures are retained and reported, never swallowed — a fallback that
  hides the reason it fell back is worse than no fallback.

## Falsifiability

The reason this plan is worth reviewing before it is built: each Done-when
clause has to become something that can *fail*.

| Clause | How it fails | What it is asserted against |
|---|---|---|
| deterministic plans | two runs differ, or a fixture's plans are not what the catalogue implies | the plan value for a fixture tree |
| irrelevant servers do not start | a Rust server is warmed in a Python-only tree | unit 2's launch log — the mock records every launch, so this is a pid assertion, not an inspection of intent |
| overrides win | a disabled or redirected server still appears | the plan value, and the launch log |
| fallback | a preferred candidate fails and no alternative is tried | the launch log (which binaries were attempted, in order) |
| explainable | a plan has no reason, or its reason does not match the evidence | every plan's reasons, per fixture |

Two consequences the plan accepts in advance:

- **"irrelevant servers do not start" is only assertable because unit 2
  records launches.** If it did not, this clause would be unfalsifiable and
  the whole unit would rest on inspection.
- **A plan is not a proof of the tree.** Fixtures are trees we constructed, so
  the tests can only show that the rules do what they say on trees we chose.
  The parent doc's comparative research is what stops those rules from being
  our invention — which is why the next section is not an appendix.

## What the research settles, and where the sources disagree

The research is **`docs/servers-list/`** — thirteen editor inventories (Eglot,
lsp-mode and its eighteen external clients, lsp-bridge, nvim-lspconfig, Helix,
vim-lsp-settings, kakoune-lsp, Kate, OpenCode, Zed's built-ins and its
extensions, mason-registry) extracted from source on 2026-09-15, merged into
2,218 normalized records, and indexed from the other end as 760 executables and
1,106 extensions.

It settles, in the parent doc's own terms:

- **catalogue contents.** Each record carries its languages and file
  extensions, the command or the source expression that computes it, its
  executables *with a confidence*, root markers, activation conditions,
  whether it runs unconfigured, and how its own source chooses among
  candidates. That is the catalogue's seed, and it is not ours to invent.
- **preference ordering**, as each source expresses it: lsp-mode's `:priority`
  plus `:add-on?`, Eglot's `alternative_index`, Helix's list position,
  vim-lsp-settings' `order_in_filetype`, Zed's `language_servers` with `...`
  and `!x`.
- **alternatives versus add-ons.** lsp-mode's main-plus-`add-on` split is
  exactly the complementary service the parent doc insists a
  `language -> executable` map cannot express, and the dataset has it as data.
- **the disagreements, which are the real decisions.** Root selection is *not*
  one rule across these sources: nvim-lspconfig takes the nearest ancestor
  carrying the highest-priority marker, with nested lists meaning equal
  priority; Helix takes the **topmost** marker between the file and the
  workspace; kakoune-lsp tries globs in order and accepts the first match
  **anywhere** above; OpenCode bounds the search by the project directory and
  skips the server outright when an exclude marker appears above; Zed uses the
  worktree and lets manifest providers refine it; lsp-bridge searches upward up
  to twenty levels for servers that cannot handle a single file. Structural
  must take a position. The plan's job is to make that position explicit and
  testable, not to average six sources into a mush that no source would
  recognise.

**What it does not settle — and the plan inherits the dataset's own honesty
about it.** The research labels its confidence, and those labels become
constraints on the catalogue:

- the Emacs sources' extensions are its weakest link: many modes were matched
  by name against another editor's table, and the record says so in
  `file_extensions_via`;
- 26 lsp-mode clients and 145 Zed extension servers have no resolved
  executable, carrying an expression or nothing;
- 64 language names that no fetched config defines (e.g. `Cpp`, `Vue`,
  `Yaml`) are recorded as unusable rather than guessed;
- VS Code, Sublime, coc.nvim and JetBrains are absent, as is runtime user
  configuration.

So the catalogue is **built from the dataset with provenance, never by copying
fields into source**. A generated entry records which record and which source
it came from, and whether that field was exact or inferred — because **a
heuristic must not be able to silently become a fact**. If a server is chosen
on an extension that was matched by name, the plan's reasons say so. That is
the same discipline as everywhere else here: an answer that cannot say where it
came from is not an answer.

Extraction is not turnkey — `scripts/` assumes the upstream repositories are
checked out as siblings — so turning the dataset into a catalogue is a build
step this unit owns. The normalized JSON is the input; `raw/` stays for the
cases the normalized record cannot express, and is not a substitute for it.

## Milestones

**A — catalogue and plans as pure policy.** The types (evidence, catalogue
entry, plan), the selection function, and fixture-tree tests asserting plans
and reasons. No spawning, no acquisition — fixtures carry explicit catalogues,
so this half needs no dataset work at all.

Then the catalogue itself, as its own step inside this milestone: a generator
that turns `docs/servers-list/servers.json` into catalogue entries **with
provenance and confidence retained**, plus the decisions the dataset's
disagreements force us to make — root selection first, since that is where the
sources differ most and where a wrong choice is expensive to unwind. The
generated catalogue is checked in, so a change to it is reviewable, and the
dataset's version is recorded with it.

**B — connect acquisition.** Stages become slots: a workspace warms what the
plans name rather than every configured adapter, which is what makes
"irrelevant servers do not start" true of the process table and not just of a
value. `structural.toml` becomes the override layer, including disabling.

**C — availability and fallback.** Attempt a plan, record failure, try the
next alternative in order, report every failure retained. Bounded by the plan
list, never by a timer.

## Cut

- **auto-installation** — out of scope in the parent doc, and a catalogue entry
  that names a command we cannot find is reported, not fetched;
- **a catalogue of everything** — a modest verified set, with broadening as
  follow-up;
- **CPU scheduling and startup staggering** — the parent doc forbids guessing
  which language an agent will use first;
- **merging overlapping roots** — `/repo` and `/repo/subproject` stay separate
  semantic owners;
- **observation** — evidence here is gathered by reading, not by watching;
  unit 4 owns watching.

## Verification

- Selection is tested on **fixture trees** with explicit catalogues and
  overrides: the plan value is the assertion, and no process starts.
- Warming is tested over unit 2's mock, which records every launch, so
  "irrelevant servers do not start" and "the fallback was tried in order" are
  pid assertions.
- Overrides are tested in both directions: an override that adds, and one that
  disables.
- Explanations are tested as data: each plan's reasons are asserted against the
  fixture's evidence, so a plan cannot be right for the wrong reason.
- `cargo test`, `cargo clippy --all-targets -- -D warnings`, `cargo doc
  --no-deps`, and the ignored real-server suite stay green at each milestone.