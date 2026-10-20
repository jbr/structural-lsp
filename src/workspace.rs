//! One workspace: a canonical root and the language servers configured for it.
//!
//! A workspace is created by the [`crate::host::Host`], which keys it by its
//! canonical root and shares it between every agent or caller bound to it.
//! Building one is cheap and starts nothing: it reads the tree's evidence
//! and `structural.toml`, asks selection what to run, and lays out one
//! [`ServerSlot`] per **planned** server (`docs/discovery.md`). Servers
//! start on demand, through the host entry — which is why a workspace can
//! exist, and answer routing questions, before anything is running.
//!
//! Laying out a slot per plan rather than per configured adapter is what
//! makes "irrelevant servers do not start" true of the process table: a
//! Python-only tree lays out no Rust slot, so no Rust server is warmed,
//! demanded, or reported as down.
//!
//! Concurrency: the host wraps this in a mutex, so document ownership is
//! serialized per workspace — the same guarantee `&mut State` gave the MCP
//! dispatcher — without serializing unrelated workspaces. Each server runs on
//! its own threads behind its handle.

use crate::clock::Clock;
use crate::config::{self, Config, Layout};
use crate::discovery::catalogue::Catalogue;
use crate::discovery::evidence::Evidence;
use crate::discovery::selection::{Selection, select};
use crate::host::observation::Pending;
use crate::lsp::adapter::{Adapter, language_for};
use crate::lsp::server::LspServer;
use anyhow::{Context, Result, bail};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

mod boundary;
mod slot;

#[cfg(test)]
mod tests;

pub use slot::{Attempt, Demand, Launch, ServerSlot, SlotState};

pub struct Workspace {
    /// The canonical workspace root, fixed when the workspace is acquired.
    /// A tool's `path` is a scope inside it; it never changes a server's root.
    root: PathBuf,
    /// One per plan, in plan order — by language, then by server — fixed
    /// when the workspace is built, and each holding that plan's whole
    /// ladder. The host entry holds the same handles, so a demand can read
    /// and move lifecycle state without this workspace's lock.
    slots: Vec<Arc<ServerSlot>>,
    /// The running server for each slot, indexed alike: `Some` exactly when
    /// that slot is `Running`. Both change together, under this workspace's
    /// lock, which is what makes the pairing safe to rely on.
    servers: Vec<Option<LspServer>>,
    /// A present but unusable `structural.toml`: nothing starts, and every
    /// call says why.
    config_error: Option<String>,
    /// What selection proposed for this tree, kept so a call can say why a
    /// server it expected is not here.
    selection: Option<Selection>,
    /// The layout the initial selection was made under: the config file's
    /// adapters and overrides. Loaded once — a `structural.toml` change
    /// still asks for a restart — but kept, so a refresh re-selects under
    /// the same policy the first selection used.
    layout: Option<Layout>,
    /// The stopped flag every slot takes, and a refresh re-takes for new
    /// slots: set by shutdown before anything is stopped, re-checked under
    /// the workspace lock before publish (`src/host/lifetime.rs`).
    stopped: Arc<AtomicBool>,
    /// Where "now" comes from for the idle clocks slots start on, and for
    /// the sweep's expiry checks (`docs/observation.md`). Injectable so
    /// tests advance time rather than sleeping through intervals.
    clock: Arc<dyn Clock>,
    /// How long a slot may sit without a strong signal before the idle
    /// sweep releases it: this workspace's `idle_eviction_secs`, or the
    /// default. Copied onto the host entry at acquisition, which is where
    /// the sweep runs from.
    idle_eviction: Duration,
    /// The filesystem events this workspace's watcher has seen and no
    /// query has been told about yet — drained at the query boundary
    /// (`docs/correctness.md`). Shared with the observation thread,
    /// which coalesces the same events into evidence refreshes
    /// (`docs/observation.md`).
    pending: Arc<Pending>,
}

impl Workspace {
    /// Read `structural.toml` and lay out a slot per configured server.
    ///
    /// Nothing is started here: a launch goes through the host entry, which
    /// can run and wait for one without holding the workspace lock. The root
    /// is canonicalized so every scope/candidate check compares real paths,
    /// not symlink spellings; if that fails (root missing) the raw root is
    /// kept for the error.
    pub(crate) fn new(
        root: PathBuf,
        stopped: Arc<AtomicBool>,
        clock: Arc<dyn Clock>,
        pending: Arc<Pending>,
    ) -> Self {
        let root = canonical_root(&root);
        let mut workspace = Self {
            root,
            slots: Vec::new(),
            servers: Vec::new(),
            config_error: None,
            selection: None,
            layout: None,
            stopped,
            clock,
            idle_eviction: config::DEFAULT_IDLE_EVICTION,
            pending,
        };
        // Loaded separately from its layout so the interval this file asks
        // for is read even when a server entry fails to parse — though a
        // file in error lays out no slots for the interval to govern.
        let config = match Config::load(&workspace.root) {
            Ok(config) => config,
            Err(error) => {
                workspace.config_error = Some(format!("{error:#}"));
                return workspace;
            }
        };
        workspace.idle_eviction = config.idle_eviction();
        let layout = match config.layout() {
            Ok(layout) => layout,
            Err(error) => {
                workspace.config_error = Some(format!("{error:#}"));
                return workspace;
            }
        };
        // Evidence is read here for the first layout. Reading it again is
        // `refresh`, driven by the workspace's observation thread: a tree
        // that grows a language is re-selected rather than stale
        // (`docs/observation.md`).
        let evidence = Evidence::gather(&workspace.root);
        let selection = select(&evidence, &Catalogue::built_in(), &layout.overrides);
        for ladder in ladders(&selection, &layout) {
            workspace.slots.push(Arc::new(ServerSlot::new(
                ladder,
                Arc::clone(&workspace.stopped),
                workspace.clock.now(),
            )));
            workspace.servers.push(None);
        }
        workspace.selection = Some(selection);
        workspace.layout = Some(layout);
        workspace
    }

    /// Re-read the tree, re-run selection, and lay out slots for languages
    /// the tree newly calls for.
    ///
    /// This is the whole reduction an event batch maps to — every batch,
    /// including an overflow, ends here — so a lost event is reconciled by
    /// the full rescan rather than trusted: nothing from the batch survives
    /// the reduction (`docs/observation.md`). The walk is bounded by
    /// [`FILE_BUDGET`](crate::discovery::evidence::FILE_BUDGET), and it
    /// holds the workspace lock only for the walk and the re-select.
    ///
    /// Existing slots are never removed here — removal is the idle sweep's,
    /// on the clock that expires a server (`docs/observation.md`), and a
    /// language whose evidence dropped keeps its slot, which serving simply
    /// stops matching. Nothing is warmed here: a
    /// slot starts when a query demands it, or — for an interested
    /// workspace — when the signal policy warms a language that is new
    /// (`docs/observation.md`). Returns the languages that are new,
    /// each paired with the index of the slot laid out for it; the pair is
    /// meaningful only while the caller still holds this lock, and it is
    /// the caller — which decides whether to warm — that maps it onto its
    /// own copy of the slot list.
    pub(crate) fn refresh(&mut self) -> Vec<(String, usize)> {
        let Some(layout) = &self.layout else {
            return Vec::new();
        };
        let old_languages: BTreeSet<&str> = self
            .selection
            .as_ref()
            .map(|selection| {
                selection
                    .plans
                    .iter()
                    .map(|plan| plan.language.as_str())
                    .collect()
            })
            .unwrap_or_default();
        let evidence = Evidence::gather(&self.root);
        let selection = select(&evidence, &Catalogue::built_in(), &layout.overrides);
        let mut new_languages = Vec::new();
        for plan in &selection.plans {
            if old_languages.contains(plan.language.as_str()) {
                continue;
            }
            // A slot that already names this plan's winner answers it — a
            // multi-language server slotted for its other language. Slotted
            // once is the initial layout's merge rule (`ladders`); this is
            // that rule applied incrementally.
            if self
                .slots
                .iter()
                .any(|slot| slot.candidate_names().contains(&plan.server()))
            {
                continue;
            }
            let ladder = ladder_for(&selection, layout, &plan.language);
            self.slots.push(Arc::new(ServerSlot::new(
                ladder,
                Arc::clone(&self.stopped),
                self.clock.now(),
            )));
            self.servers.push(None);
            new_languages.push((plan.language.clone(), self.slots.len() - 1));
        }
        self.selection = Some(selection);
        new_languages
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn slots(&self) -> &[Arc<ServerSlot>] {
        &self.slots
    }

    /// How long a slot may sit without a strong signal before the idle
    /// sweep releases it. The entry copies this at acquisition, so the
    /// sweep's park arithmetic never needs this workspace's lock.
    pub(crate) fn idle_eviction(&self) -> Duration {
        self.idle_eviction
    }

    pub fn config_error(&self) -> Option<&str> {
        self.config_error.as_deref()
    }

    /// The running server in `index`'s slot.
    ///
    /// Every index a caller can obtain comes from [`Workspace::select`] or
    /// [`Workspace::index_of`], both of which yield running slots only, so
    /// this is an error only when a caller invents an index.
    pub fn server(&self, index: usize) -> Result<&LspServer> {
        self.servers
            .get(index)
            .and_then(Option::as_ref)
            .with_context(|| self.no_such_server(index))
    }

    pub fn server_mut(&mut self, index: usize) -> Result<&mut LspServer> {
        // Checked first, because naming the slot in the error needs an
        // immutable look at `self` that a held `&mut` would forbid.
        if self.servers.get(index).and_then(Option::as_ref).is_none() {
            bail!("{}", self.no_such_server(index));
        }
        Ok(self
            .servers
            .get_mut(index)
            .and_then(Option::as_mut)
            .expect("checked immediately above"))
    }

    /// Put a freshly started server in its slot. The slot's *state* is the
    /// launcher's to move, through the [`Launch`] guard it already holds.
    pub fn publish(&mut self, index: usize, server: LspServer) {
        if let Some(slot) = self.servers.get_mut(index) {
            *slot = Some(server);
        }
    }

    /// The slot running the server named `name`, when one is running.
    pub fn index_of(&self, name: &str) -> Option<usize> {
        self.slots
            .iter()
            .position(|slot| slot.name() == name)
            .filter(|index| self.servers[*index].is_some())
    }

    /// The running slots a query should go to: the ones serving `scope`'s
    /// extension when the scope is a file of a known language, otherwise
    /// every running slot. An actionable error when none of them is running,
    /// naming what each configured server is doing instead.
    pub fn select(&self, scope: Option<&Path>) -> Result<Vec<usize>> {
        let candidates = serving(&self.slots, scope);
        if candidates.is_empty() {
            // A config that will not parse means nothing was configured, so
            // the answer is about the file rather than about this scope.
            if self.config_error.is_some() {
                bail!("{}", self.nothing_running());
            }
            bail!(
                "no language server here serves `{}` files.\n  here: {}\n{}",
                scope
                    .and_then(Path::extension)
                    .unwrap_or_default()
                    .to_string_lossy(),
                self.names(),
                self.not_planned()
            );
        }
        let running = self.running_of(&candidates);
        if running.is_empty() {
            // A file names the servers that should have served it; a bare
            // scope has nothing to name but the whole configuration.
            if scope.is_some_and(|scope| scope.is_file()) {
                bail!("{}", self.servers_down(&candidates));
            }
            bail!("{}", self.nothing_running());
        }
        Ok(running)
    }

    /// Every running server, for the questions a view asks about what is
    /// currently offered — adapters answer for what is merely configured.
    pub fn running(&self) -> impl Iterator<Item = &LspServer> {
        self.servers.iter().flatten()
    }

    /// One line per running server, for result footers.
    ///
    /// A server that is running because the candidate above it would not
    /// start says so here. That is the only place a *successful* fallback
    /// is visible — every other report is about something being wrong —
    /// and leaving it out would let the ladder silently substitute one
    /// server for another.
    pub fn descriptions(&self) -> String {
        self.servers
            .iter()
            .enumerate()
            .filter_map(|(index, server)| Some((index, server.as_ref()?)))
            .map(|(index, server)| {
                let fallen_from = self.slots[index].failed_attempts();
                if fallen_from.is_empty() {
                    return server.description();
                }
                format!(
                    "{} (after {})",
                    server.description(),
                    fallen_from
                        .iter()
                        .map(|attempt| format!("{} failed to start", attempt.server))
                        .collect::<Vec<_>>()
                        .join(", ")
                )
            })
            .collect::<Vec<_>>()
            .join(" · ")
    }

    /// Stop a slot's server and forget it, leaving the slot cold for a later
    /// demand. One operation, so shutdown and eviction cannot disagree about
    /// what stopping means — and so a session that outlives the host finds a
    /// cold slot rather than a handle to a server that is gone.
    pub fn stop(&mut self, index: usize) -> Option<LspServer> {
        let server = self.servers.get_mut(index).and_then(Option::take);
        if let Some(slot) = self.slots.get(index) {
            slot.mark_cold();
        }
        server
    }

    /// Take every server out, leaving every slot cold, and hand them back so
    /// the caller can reap them **outside** this workspace's lock. Dropping a
    /// server joins its threads — bounded, but not instant — and this lock is
    /// the one a launcher needs to publish, so reaping here would make every
    /// shutdown hold it for as long as the teardown takes.
    pub fn take_all(&mut self) -> Vec<LspServer> {
        (0..self.slots.len())
            .filter_map(|index| self.stop(index))
            .collect()
    }

    /// Stop every slot the idle sweep has expired: `Running`, `Starting`,
    /// and `Failed` slots alike, each on its own clock — the last strong
    /// signal addressed to it plus the interval. A `Starting` slot whose
    /// launch outlives the interval is the one case that makes an in-flight
    /// launch stale, and the generation the launch was elected under is
    /// what its publish then checks (`docs/observation.md`). `Cold` slots
    /// have nothing to stop. Returns the servers so the caller can drop
    /// them outside this workspace's lock, as `take_all` does.
    pub(crate) fn evict_idle(&mut self, now: Instant) -> Vec<LspServer> {
        let idle = self.idle_eviction;
        let expired = |slot: &ServerSlot| {
            now.checked_duration_since(slot.last_use())
                .is_some_and(|held| held >= idle)
        };
        let expired_indices: Vec<usize> = (0..self.slots.len())
            .filter(|&index| {
                !matches!(self.slots[index].state(), SlotState::Cold) && expired(&self.slots[index])
            })
            .collect();
        expired_indices
            .into_iter()
            .filter_map(|index| self.stop(index))
            .collect()
    }

    /// Which of `candidates` have a server, read from the workspace's own
    /// copy: while this lock is held, a server in `servers` *is* a running
    /// server. The slot's state is for readers that hold no workspace lock.
    fn running_of(&self, candidates: &[usize]) -> Vec<usize> {
        candidates
            .iter()
            .copied()
            .filter(|index| self.servers[*index].is_some())
            .collect()
    }

    /// The servers this workspace has no slot for, and why selection left
    /// each one out.
    ///
    /// Without this a configured server simply vanishes from the message.
    /// The case is ordinary: a tree with no Python lays out no Python
    /// slot, and then a `.py` file appears — the file is real, `pyright`
    /// is configured, and "no language server serves `py` files" would be
    /// true and useless. Noticing the new language while the workspace is
    /// live is observation's
    /// (`docs/observation.md`); saying so is this.
    fn not_planned(&self) -> String {
        let Some(selection) = &self.selection else {
            return String::new();
        };
        selection
            .declined
            .iter()
            .map(|declined| format!("  not here: {}\n", declined.explain()))
            .collect()
    }

    fn names(&self) -> String {
        self.slots
            .iter()
            .map(|slot| slot.name())
            .collect::<Vec<_>>()
            .join(", ")
    }

    fn no_such_server(&self, index: usize) -> String {
        match self.slots.get(index) {
            Some(slot) => format!(
                "{} is {}, so it cannot answer this query",
                slot.name(),
                slot.state().label()
            ),
            None => format!("no such language server slot: {index}"),
        }
    }

    /// What to say when the servers that would serve a file are not running:
    /// each one's state, and how to fix the ones that failed.
    fn servers_down(&self, candidates: &[usize]) -> String {
        let mut text = String::new();
        for &index in candidates {
            let slot = &self.slots[index];
            let state = slot.state();
            text.push_str(&format!("  {}: {}", slot.name(), state.label()));
            if let Some(reason) = state.reason() {
                text.push_str(&format!(
                    ": {reason}; to fix: {}",
                    slot.adapter().install_hint()
                ));
            }
            text.push('\n');
            text.push_str(&tried(slot));
        }
        text
    }

    fn nothing_running(&self) -> String {
        let mut text = String::from("no language server is running.\n");
        if let Some(error) = &self.config_error {
            text.push_str(&format!("  structural.toml: {error}\n"));
        }
        for slot in &self.slots {
            let state = slot.state();
            text.push_str(&format!("  {}: {}", slot.name(), state.label()));
            if let Some(reason) = state.reason() {
                text.push_str(&format!(
                    "; to fix: {}",
                    slot.adapter().install_hint().as_str()
                ));
                text.push_str(&format!("\n    {reason}"));
            }
            text.push('\n');
            text.push_str(&tried(slot));
        }
        text.push_str("Restart this server after installing or configuring one.");
        text
    }
}

/// The ladder for each plan, in plan order: the adapter that will run each
/// candidate, preferred first.
///
/// Two things happen here that a plan cannot do for itself. A candidate is
/// resolved to an adapter — from the [`Layout`] when the file or a built-in
/// describes it, and from the catalogue entry otherwise, which is the only
/// way an alternative this project never configured can be started at all.
/// And two plans naming the same server become **one** slot, because one
/// server is one process however many languages it was planned for.
///
/// When that happens the ladder is the **intersection** of the two, in the
/// first plan's order: falling back to a candidate that serves only one of
/// the languages would quietly drop the other, and a slot that answers for
/// less than it was laid out for is worse than one with a shorter ladder.
fn ladders(selection: &Selection, layout: &Layout) -> Vec<Vec<Arc<dyn Adapter>>> {
    let mut ladders: Vec<Vec<Arc<dyn Adapter>>> = Vec::new();
    for plan in &selection.plans {
        let ladder = ladder_for(selection, layout, &plan.language);
        match ladders
            .iter_mut()
            .find(|existing| existing[0].name() == ladder[0].name())
        {
            Some(existing) => {
                existing
                    .retain(|adapter| ladder.iter().any(|other| other.name() == adapter.name()));
            }
            None => ladders.push(ladder),
        }
    }
    ladders
}

/// The ladder one language calls for, in the plan's candidate order: the
/// layout's adapter where the configuration defines or overrides one, the
/// catalogue entry's own command elsewhere.
///
/// Its own function because `refresh` needs one language's ladder without
/// merging (`Workspace::refresh`), while the initial layout merges across
/// languages (`ladders`).
fn ladder_for(selection: &Selection, layout: &Layout, language: &str) -> Vec<Arc<dyn Adapter>> {
    selection
        .plans
        .iter()
        .find(|plan| plan.language == language)
        .map(|plan| {
            plan.candidates
                .iter()
                .map(|entry| {
                    layout
                        .adapter(&entry.name)
                        .unwrap_or_else(|| config::adapter_for(entry))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The candidates a slot tried and rejected, indented under it. Empty for a
/// slot that never fell back, so an ordinary failure reads as it always did.
fn tried(slot: &ServerSlot) -> String {
    slot.failed_attempts()
        .iter()
        .map(|attempt| format!("    tried {}\n", attempt.explain()))
        .collect()
}

/// The slots that would serve `scope`: the ones whose adapter knows the
/// file's language, or every slot when the scope names no file.
///
/// One rule, in one place, because two callers need it for different reasons
/// — a launch asks which servers to start, a query asks which to send to —
/// and they must never disagree about it.
pub fn serving(slots: &[Arc<ServerSlot>], scope: Option<&Path>) -> Vec<usize> {
    let all = || (0..slots.len()).collect::<Vec<usize>>();
    let Some(scope) = scope.filter(|scope| scope.is_file()) else {
        return all();
    };
    let serving: Vec<usize> = (0..slots.len())
        .filter(|index| language_for(&slots[*index].adapter().languages(), scope).is_some())
        .collect();
    if serving.is_empty() && scope.extension().is_none() {
        // A file of no known language and no extension leaves nothing to
        // route by, so every server is a candidate, as it always was.
        return all();
    }
    serving
}

/// The canonical spelling of a root, so two callers naming the same tree
/// through different paths — a symlink, a relative path, a trailing slash —
/// land on one workspace. A root that cannot be canonicalized (it does not
/// exist) is kept as given, for the error to report.
pub fn canonical_root(root: &Path) -> PathBuf {
    std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf())
}
