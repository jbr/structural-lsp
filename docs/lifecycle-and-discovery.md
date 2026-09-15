# Shared host, server lifecycle, and project discovery

Plan agreed 2026-09-15. This is the next substantial development direction
following slice 4 and the findings in `user-test-report.md`. It records the
intended architecture and a sequence of independently reviewable work units;
it is not a description of features already implemented.

## Why this next

The current service has useful semantic tools, but its ownership and activation
model is rudimentary: one fixed workspace, with rust-analyzer and pyright plus
configured generic servers started eagerly whether or not they are relevant.
Adding more views and actions before fixing that foundation would increase the
migration cost.

Two needs meet here:

- Real use exposed a freshness gap: changes to unopened workspace files can be
  absent from bare-name queries. Refreshing previously opened documents is not
  workspace-wide synchronization (report C2).
- The intended embedded consumer, efference, runs multiple agents in one host
  process. Those agents should share language servers instead of launching a
  separate set per agent.

This is a substantial subsystem, not one refactoring commit. Separate ownership,
lifecycle, discovery policy, filesystem observation, and protocol synchronization
so each can be tested independently.

## Current implementation baseline

At the time of this plan:

- `src/state.rs` owns one canonical root and a vector of language servers. Calls
  receive `&mut State`; document ownership is serialized by the MCP dispatcher.
- `State::new` starts every built-in and configured generic server. Startup is a
  sequential loop, waiting for each server's initialization response.
- `src/config.rs` loads root-local `structural.toml`. Built-ins can be overridden,
  and generic servers added, but built-ins cannot be disabled.
- File extensions route queries among already-running servers; they do not
  determine which servers start. Non-file scopes generally query all servers.
- `src/main.rs` chooses cwd or `STRUCTURAL_ROOT`. A tool's `path` is a scope
  within that fixed root, not a workspace selector.
- `src/lib.rs` exposes the tools and state for in-process tests, not yet a shared
  embedded host interface.
- Each `LspServer` owns its process and opened-document snapshots. Owned documents
  are refreshed before queries. There is no client-owned filesystem observation,
  workspace idle eviction, or demand-driven reacquisition.
- Capability registration requests are acknowledged without retaining their
  registrations. Watched-file dynamic registration is not currently advertised;
  implementing it requires real registration state, not just a watcher callback.

Historical documents, notably `docs/mcp.md` and parts of `design.md`, describe
older startup and readiness behavior. This plan supersedes their lifecycle
sketches as future intent; the source remains the authority for current behavior.

## Consumers and ownership

One structural host serves two first-class use cases:

1. **Standalone MCP:** the MCP process owns a host, a default workspace, and its
   child servers. Independently spawned MCP processes do not share LSP processes.
2. **Embedded efference:** the application owns one long-lived host shared by its
   agents. Every agent has an effective cwd even when the application's process
   cwd is elsewhere. Agent start/end hooks communicate that interest.

The embedded API may be designed specifically for efference's actual usage. Do
not invent a universal client/session framework to accommodate hypothetical
consumers. MCP is another front end to the same machinery, not a second lifecycle
implementation.

Conceptual ownership:

```text
MCP caller / embedded agents (each with a default workspace context)
                         |
                  structural host
                         |
                 workspace sessions
                 - discovery evidence
                 - observation subscriptions
                 - activation policy
                 - language-server instances
                   - document and protocol state
```

The initial sharing unit is a workspace and its effective server configuration.
Agents using the same canonical workspace and configuration share instances.
Unrelated projects do not share a single process merely because they use the same
language. Toolchains, environments, initialization options, and discovered project
roots matter to instance identity.

Overlapping filesystem trees do not imply interchangeable semantic roots. Keep
three concepts separate:

- the workspace selected by an agent;
- project roots discovered within it;
- filesystem coverage needed for observation.

Do not automatically merge `/repo` and `/repo/subproject` LSP sessions. Observation
resources may be shared without merging semantic ownership. Exact project-root
selection rules await comparative research.

Initially serialize operations within a workspace, preserving today's document
state and rename/query ordering assumptions. Independent workspaces must be able
to proceed independently; do not hold a global host lock through semantic work.
Host shutdown must deterministically clean up child processes.

## Interest, observation, and server residency

These are separate lifetimes:

- **Agent interest:** an agent is associated with this workspace.
- **Filesystem observation:** structural retains discovery evidence and a change
  subscription for a tree.
- **Server residency:** a particular server is starting, running, or stopping.

An observed workspace need not have any language servers running. Observation
retains lightweight evidence, not a source index or every document's contents.
A workspace can have Rust warm, Python stopped, and TypeScript newly discovered;
server lifecycle is per instance, not one workspace-wide warm/cold flag.

Useful transitions include:

- Agent start acquires the workspace, discovers projects, and warms applicable
  servers.
- Agent end releases that agent's interest, not another agent's session.
- After a generous idle interval, servers can stop while observation remains.
- A later call reacquires the required servers and waits for readiness.
- Observation is eventually released according to a separate retention policy.

An attached but idle agent does not have to pin processes indefinitely. In-flight
operations do prevent eviction. A dropped or missed agent-end signal must not be
the sole determinant of cleanup.

A server idle interval around 30 minutes is a reasonable starting default, not a
final tuning result. Observation should have a separate, longer retention policy
and eventual bound; its exact duration is open.

### Strong and weak signals

Strong signals are agent start, an explicit tool query, and an explicit host
request to warm a workspace. Filesystem changes are weak signals: they always
update relevant discovery/activity evidence but need not cause process startup.

Suggested default policy to validate during implementation:

| Event | Effect |
|---|---|
| Agent starts | Discover and eagerly warm applicable servers |
| Query arrives after eviction | Start required servers and wait for readiness |
| New language appears with an attached agent | Warm its selected server |
| Relevant edit after eviction, with an attached agent | May reactivate the relevant server |
| Change in an observed-only workspace with no attached agent | Update evidence, do not automatically launch |

Filesystem activity must not be an unconditional keepalive. Build output,
background generators, and server-induced changes can otherwise keep processes
alive or resurrect them forever. Filtering and activity policy need experiments;
the architecture must not depend on perfect inference of intent.

The new idle timer is unrelated to the existing **protocol inactivity backstop**.
The former releases unused resources; the latter detects a stalled server while
work is pending. Clarify the project's “no timers except the backstop” convention
when adding lifecycle expiry. It still prohibits settle sleeps and timer-based
claims of semantic readiness.

## Default activation: eager, based on evidence

Prefer optimistic warming over maximal laziness. rust-analyzer can take minutes
to become useful, and an agent may only read code; waiting for the first file edit
would miss that session entirely.

On workspace acquisition:

1. Discover language and project evidence.
2. Choose applicable, available servers.
3. Begin starting them immediately and independently.

A filesystem change is an additional activation signal, especially for a newly
introduced language, not a prerequisite for startup. An explicit file query must
also trigger selection/activation without waiting for a watcher to notice it.
Bare-symbol queries have no language hint and must not search only servers that
happened to be activated by earlier file queries.

Do not initially add CPU scheduling, startup staggering, or guesses about which
language an agent will use first. Independent initialization must not prevent
other servers from launching. Retain observable startup/readiness/failure status;
a resource limit can be added later if experience warrants it.

## Zero-configuration discovery and selection

Known languages should work with installed servers without requiring a project
config. Configuration overrides the defaults or adds an unknown server; it is not
the normal admission ticket for a language. Automatic installation is out of scope.

Separate these layers:

| Layer | Responsibility |
|---|---|
| Project discovery | Source/manifest evidence and candidate project roots |
| Server catalogue | Commands, languages, applicability, preference, alternatives |
| Specialized adapter | Server-specific protocol behavior, readiness, extensions |
| Workspace session | Select and run instances through the lifecycle machinery |

Most catalogue entries should use the generic LSP path. A broad catalogue does
not require a handwritten adapter per server. Represent servers covering several
languages, alternative implementations, and intentionally complementary services;
`language -> executable` is not sufficient for every case.

Selection should be deterministic and explainable: evidence plus catalogue plus
explicit overrides yields proposed server instances and reasons. Availability
includes successful initialization, not just finding an executable. A failed
candidate may lead to trying an alternative, retaining the failure in status;
this is bounded selection, not an indefinite restart loop.

Project markers should influence preference only where the relationship is
verified. For example, a runtime configuration file does not by itself prove the
existence or suitability of a particular language-server command.

Likely activation vocabulary is disabled/automatic/eager, but exact configuration
syntax and default precedence rules are not settled. Preserve the distinction
between available definitions, applicable services, and actually running instances.

### Comparative research (operator-owned)

The operator will research Eglot, lsp-mode, lsp-bridge, and comparable editor
projects. Useful outputs are concrete rules, disagreements, and reusable data,
not necessarily a comprehensive census:

- root selection, nested projects, monorepos, and competing root markers;
- preference ordering and project-specific overrides of that ordering;
- command availability, project-local executables, wrappers, and failure fallback;
- multi-language servers versus alternative and complementary servers;
- user overrides for inferred roots, detection, and server choice;
- reusable libraries/catalogues and their licensing.

Investigate whether libraries cover inventory, language identification, or root
recognition. Do not assume there is a universal discovery library: server choice
and root selection contain policy that structural may need to own.

## Workspace and language routing

Every caller has a default workspace context:

- MCP: normally its startup directory (or explicitly configured root).
- Embedded: the calling agent's effective cwd, supplied by efference.

Never change process cwd or a global current-workspace variable to implement this.
An optional `workspace` tool argument selects another workspace for that call;
`path` remains a scope within the selected workspace.

Provide an optional `language` routing constraint rather than routinely requiring
server identity. Agents generally know Rust or Python, not which implementation
was selected by the catalogue. Server/service identity can be an advanced escape
hatch if an actual ambiguity requires it.

Default routing:

- A file path or anchor infers applicable services from the file and project.
- A bare symbol, optionally directory-scoped, queries applicable selected services
  for that scope.
- An explicit language narrows those services.
- Cross-language collisions remain explicit ambiguity, never silent selection.

Argument requirements must not depend on how many servers happen to be running.
Creating a new-language file or completing startup must not invalidate a previously
valid call. Omission may mean waiting for multiple relevant servers; do not silently
query only the ready subset and present it as a whole-workspace answer.

## Observation and freshness are related, not equivalent

A shared observation service can route events to overlapping subscriptions.
Optimal native-watch deduplication is an optimization, not a correctness
prerequisite. Inactive observation is relatively cheap, not free: recursive watch
costs vary by platform, initial inventory can be expensive, and events may overflow.

Discovery exclusions and LSP watch subscriptions are distinct filters. A language
server may need changes to manifests, configuration, or generated files excluded
from ordinary source discovery.

Protocol synchronization must account for:

- opened documents, refreshed through document synchronization;
- unopened files and metadata, notified through applicable watched-file behavior;
- actual registration/unregistration and watch patterns;
- create/delete/rename events and structural's own writes;
- reconciliation after observation gaps or overflow;
- a defined query boundary for pending filesystem changes.

Retained watcher history is not a complete semantic replay log. Restarted servers
initialize from the current filesystem. Observation interrupted or invalidated by
overflow must be reconciled rather than blindly trusted.

Most importantly, notification delivery and message ordering do not universally
prove that a server's semantic index has caught up. Observed progress and semantic
readiness require server-specific validation. Do not declare C2 fixed merely
because a watcher exists, or maintain unconditional “every answer is current”
wording beyond what the implementation can support.

Disabling servers' own watchers, where appropriate, is adapter-specific. Owning
observation does not universally eliminate server-side watching.

## Sequenced implementation units

These are work units, not a demand that each fit in one commit. Each should leave
a usable, tested system and avoid bundling the next unit's policy into its refactor.

### 0. Record the contract and establish regressions

This document records the agreement. Before architecture changes, establish cases
for unopened-file edits and new files followed immediately by bare-name queries.
Initially these may be documented reproductions or explicitly pending tests; do
not represent the baseline as passing. Preserve existing rename preview/apply
safety coverage and add sharing/isolation acceptance tests as the API appears.

An independent small fix can land here: file-scoped name lookup should nominate
from the file's document-symbol tree instead of merely filtering workspace-symbol
results (report C1). Investigate the raw responses before assuming the broader
qualified-name failure is caused by a server-side cap; the report establishes an
inadequate candidate pool, not that particular mechanism.

Run a small real-server freshness/watch experiment early, in parallel with host
work, to inform later interfaces. It should test unopened edits and newly created
files without preliminary outline calls or arbitrary sleeps.

### 1. Extract the shared host; preserve current server selection

Establish host/workspace ownership and efference-shaped agent start/end and caller
context. Initially keep today's configured-server selection behind that boundary.
Wire MCP's default context through the same host without yet adding cross-workspace
tool arguments. Serialize within a workspace, not globally.

**Done when:** two clients share one launch; releasing one leaves the other
functional; independent workspaces have independent defaults and make progress
independently; host shutdown reaps children.

**Not included:** automatic discovery, native observation, idle eviction, or a
large catalogue. Avoid inventing a general-purpose session framework.

### 2. Make server lifecycle explicit

Implement starting/running/failed/stopping state, deduplicated acquisition during
startup, independent launches, in-flight protection, idle expiry, controlled
reacquisition, and explanatory status. Agent interest no longer implies permanent
server residency.

Use an injectable clock and mock processes/events. Advance time in tests instead
of sleeping through lifecycle intervals. Strong activation signals are sufficient
for this unit; filesystem activity is not yet part of the policy.

**Done when:** concurrent demand causes one launch; expiry cannot interrupt an
operation; later demand starts a fresh instance; one failing server does not block
another; shutdown during startup leaves no orphan.

### 3. Add discovery and selection as pure policy

Use the comparative research to implement evidence + catalogue + overrides ->
server-instance plans with explanations. Keep selection independently testable
without spawning. Then connect acquisition to inventory and eager warming.

Begin with a modest verified catalogue and explicit supported root-routing cases,
not an unvalidated wholesale import. Broadening declarative entries should become
ordinary follow-up work.

**Done when:** fixture trees produce deterministic plans; irrelevant servers do
not start; overrides win; an unavailable preferred candidate can fall back to an
available alternative; selection and project-root choices are explainable.

### 4. Add observation and incremental discovery

Define an event-source interface and test with synthetic events before adding the
native backend. Maintain discovery evidence, identify new languages/projects,
retain observation across server eviction, implement strong/weak signal policy,
reconcile missing events, and eventually release observations.

Route events correctly across overlapping subscriptions; optimal native-watch
sharing can follow later.

**Done when:** a new language activates in an interested workspace; an observed-only
workspace updates evidence without automatically launching; server eviction leaves
observation intact; lost events cause reconciliation.

This unit does **not** yet establish semantic freshness.

### 5. Connect observation to LSP synchronization

Implement the protocol responsibilities described above, including registration
state and coordination with document ownership and structural's own writes.
Define the query synchronization boundary and validate it against real servers.
The early experiment should inform this work, not substitute for acceptance tests.

**Done when:** supported real-server cases answer immediate queries after external
unopened-file edits and file creation without outlining first or sleeping to
settle. Any remaining server limitation is explicit, not hidden by a claim of
currentness. Keep protocol-backstop behavior distinct from resource expiry.

### 6. Expose multi-workspace MCP routing and integrate efference

Add optional workspace selection and language constraints, preserving inference and
stable argument requirements. Provide useful errors/status for unavailable,
starting, running, and stopped services. Exercise actual effective-cwd and agent
lifecycle behavior through efference.

**Done when:** an MCP mount can inspect another project without changing another
caller's default; embedded agents share one host; returning to an expired workspace
reacquires the required servers.

A management tool is not automatically required. Decide whether explicit release
or status operations add value after lifecycle behavior exists; agents must not
need to babysit server processes for normal use.

## Testing and scope discipline

Use pure fixture-driven tests for discovery/selection, synthetic events for
observation, and fake clocks plus mock servers for lifecycle races. Reserve real
servers for protocol and semantic guarantees. Follow existing conventions: tests
that mutate use scratch copies, and acceptance must cover realistic common-name
and multi-project cases rather than only tiny happy-path fixtures.

Keep the existing verification suite passing as ownership moves:

```sh
cargo test
cargo test -- --ignored
cargo clippy --all-targets -- -D warnings
cargo doc --no-deps
```

Explicitly out of scope for this sequence:

- an IPC daemon or cross-process MCP sharing;
- automatic language-server installation;
- sophisticated CPU scheduling or startup staggering;
- automatically merging overlapping semantic project roots;
- concurrent semantic operations within one workspace;
- a broad rewrite of rendering or all user-report ergonomics.

The report remains useful follow-up work. Intra-doc link renaming belongs to the
language server: apply its edits faithfully, do not add a textual rename pass.
File-scoped targeting is a small independent fix. Reference pagination/filtering,
normalization, and improved messages need not block the host architecture.

## Remaining decisions

The architecture above is agreed; these details remain to be specified from
research and implementation evidence:

- exact project-root and nested-project routing rules;
- catalogue contents, preference rules, and complementary-service representation;
- configuration syntax, effective-config identity, and override precedence;
- weak-activity filtering and activation details;
- final server-idle and observation-retention defaults;
- the tested freshness contract and server-specific limitations;
- whether operational status needs a dedicated MCP tool.

The next implementation task is shared-host extraction, not a simultaneous rewrite
of discovery and synchronization. Catalogue research and the early freshness
experiment can proceed alongside it.
