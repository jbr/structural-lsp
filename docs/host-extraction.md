# Unit 1: extract the shared host

Breakdown of work unit 1 in `lifecycle-and-discovery.md` ("Extract the shared
host; preserve current server selection"). It is a plan, not a record of
implemented behavior; the source is the authority.

**Done when** (from the parent doc):

- two clients share one launch;
- releasing one leaves the other functional;
- independent workspaces have independent defaults and make progress
  independently;
- host shutdown reaps children.

**Not included:** automatic discovery, native observation, idle eviction, a
large catalogue, a general-purpose session framework.

## Shape

```text
MCP front end            embedded agents
  AgentId("mcp")           AgentId(<agent>)
        \                     /
         \                   /
            Host  (Arc)
              - workspaces: Mutex<BTreeMap<canonical root, Arc<Entry>>>
              - agent_started / agent_ended
                   |
              Entry (shared, one per workspace)
              - workspaces: Mutex<Workspace>
              - interested: Mutex<BTreeSet<AgentId>>
                   |
              Workspace  (= today's State)
              - root, servers, unavailable, config_error
              - select / descriptions / nothing_running
                   |
              LspServer (unchanged)
```

`Session` is the mcplease `State` type: a caller's binding to one workspace.
It holds the host and the entry, and hands tools the workspace guard. Tool
bodies take `&mut Workspace`; only the three `Tool<Session>` entry points lock.

## Locking rule

Serialize within a workspace, not globally.

- The host's map lock is taken to look up, create, or release an entry — and
  for a *new* workspace it is held across `Workspace::new`, which starts
  servers and waits for `initialize`. No semantic work runs under it.
- Semantic work takes the entry's own `Mutex<Workspace>`.
- Two sessions on different workspaces can hold their guards at once: an
  operation never takes the map lock at all.

Holding the map lock across startup is deliberate for this unit. It makes "two
clients share one launch" true by construction, and it is what keeps an entry
from ever sitting in the map with no interested agent. The cost is not just
"two clients share one launch": a concurrent `agent_started` for *any* root
waits behind an in-flight startup — bounded by the inactivity backstop, 90 s per
configured server, serially — and while it waits so do `agent_ended` and
`shutdown`. Unit 2 replaces this with an acquisition that does not hold the
global lock during startup.

## Interest and release

- `agent_started(id, cwd)` canonicalizes `cwd`, acquires the entry, and records
  interest under the map lock, so no entry can sit in the map with an empty
  interest set while it is being bound.
- `agent_ended(id)` drops that agent's interest. When the set is empty the map
  drops its `Arc` for the entry; any live `Session` keeps the entry — and its
  servers — alive until it is dropped. In-flight work therefore cannot be
  interrupted by a release, with no explicit guard.
- A released entry is also recorded *weakly* in a detached list, so `shutdown`
  can still stop it. Without that, a workspace released while a session still
  held it would be invisible to `shutdown`, and its children would live until
  that session happened to drop — which in a long-lived host may be never.
- No timers. Idle expiry and "interest no longer implies residency" are unit 2.
- `Host::shutdown` drains the map and the detached list and shuts every
  workspace's servers down, so children are reaped deterministically whether or
  not an agent still claims the workspace.

## Commits

1. **`Workspace` out of `State`.** Move `src/state.rs` to `src/workspace.rs`;
   `State` becomes `Workspace`, identical behavior. Tool helpers and `show`
   views take `&mut Workspace`.
2. **`Host` + `Session`.** `src/host.rs` (host, entry, agent interest,
   shutdown) and `src/session.rs`. `Session` replaces `State` as the mcplease
   type parameter; `main.rs` builds a host and acquires the MCP default
   context through it.
3. **Test scaffolding.** Lift the Python mock and interpreter finder out of
   `src/lsp/handle/tests.rs` into a shared test-support module; teach the mock
   to record each launch to a path given as an extra argv.
4. **Host tests.** Unit tests over the mock: one launch for two agents on one
   root; a release leaves the other agent working; two roots hold their
   workspace locks at once; shutdown reaps children; a bad config still yields
   an actionable error. Acceptance coverage in `tests/mcp_smoke/` keeps the
   existing per-root `Session::new` idiom.

## Deliberately deferred

- **Effective-config identity.** `structural.toml` is loaded from the root
  today, so the canonical root already determines the effective configuration
  and is a sufficient key. The key grows a config component when a config can
  come from somewhere other than the workspace root.
- **Per-call workspace selection.** MCP has one default context; the `workspace`
  argument and `language` constraint are unit 6.
- **Discovery.** `Workspace::new` still selects today's built-ins plus
  `structural.toml` entries, eagerly. The catalogue is unit 3.
