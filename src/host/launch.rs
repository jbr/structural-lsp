//! Starting one planned server: electing a launcher, walking the plan's
//! ladder, and publishing what initializes.
//!
//! Split from [`super`] because this is where the lock discipline lives.
//! Nothing here holds the workspace lock while it spawns or handshakes —
//! only publication takes it, once — and the ladder walk
//! (`docs/discovery.md`) is what makes that discipline worth stating
//! separately: one launcher may start and stop several children before one
//! of them is published.

use super::{Entry, Registration};
use crate::lsp::adapter::Adapter;
use crate::lsp::server::LspServer;
use crate::workspace::{Demand, Launch};
use anyhow::{Result, bail};
use std::path::Path;
use std::sync::Arc;

/// Why one rung of the ladder produced no server.
enum Unstarted {
    /// This candidate did not start: its command would not spawn, or the
    /// handshake failed. The next one on the ladder gets a turn.
    Unavailable(String),
    /// Not the candidate's failure: the host is stopping, so no candidate
    /// would start and the walk ends.
    Refused(String),
}

impl Entry {
    /// Make the servers that would answer for `scope` running, starting the
    /// ones that are not.
    ///
    /// No workspace lock is held here, deliberately: a launch publishes
    /// *under* that lock, so waiting for one while holding it would deadlock
    /// against the very launcher this call elects.
    pub(crate) fn activate(self: &Arc<Self>, scope: Option<&Path>) {
        // Start every needed launch first and wait afterwards. Waiting as we
        // went would serialize exactly what giving each launch a thread was
        // for: one server's `initialize` would still be a delay to the next.
        for index in self.warm(scope) {
            let _ = self.slots[index].wait_for_launch();
        }
    }

    /// Start the servers that would answer for `scope` without waiting for
    /// any of them, and say which ones were wanted.
    ///
    /// This is binding a session, not answering a query: a caller that has
    /// not asked anything yet has nothing to be correct about, and every
    /// query goes through [`Entry::activate`], which waits. Waiting here
    /// instead would put a whole `initialize` plus index warm-up
    /// (`../docs/servers.md`) in front of the front end's first byte — long
    /// enough on a cold index for an MCP client to give up on the handshake.
    pub(crate) fn warm(self: &Arc<Self>, scope: Option<&Path>) -> Vec<usize> {
        let wanted = crate::workspace::serving(&self.slots, scope);
        for &index in &wanted {
            // Best effort: a server that will not start is recorded on its
            // own slot, and the call goes on to ask for what is available.
            // Bailing here would let one broken server stop every other from
            // being warmed at all.
            let _ = self.start(index);
        }
        wanted
    }

    /// Bring one slot to `Running`, or say why it cannot be.
    ///
    /// A demand that finds the slot already starting joins that launch rather
    /// than beginning a second one, which is what makes concurrent demand
    /// produce exactly one process.
    fn start(self: &Arc<Self>, index: usize) -> Result<()> {
        let guard = self.lifetime.admit()?;
        match self.slots[index].demand() {
            // Already running, or someone else's launch is under way; either
            // way the wait in `activate` covers it.
            Demand::Ready | Demand::Join => Ok(()),
            Demand::Failed(reason) => bail!("{reason}"),
            Demand::Refused => bail!("the host is shutting down"),
            Demand::Lead(launch) => {
                self.spawn_launch(index, launch, guard);
                Ok(())
            }
        }
    }

    /// Start one server and publish it.
    ///
    /// The spawn and the `initialize` handshake hold no lock at all, so other
    /// workspaces — and this one's other slots — keep working while it runs.
    /// Only publication takes the workspace lock, and it takes it once, so no
    /// caller can observe a `Running` slot whose server is not yet in place.
    fn spawn_launch(self: &Arc<Self>, index: usize, launch: Launch, guard: swansong::Guard) {
        let entry = Arc::clone(self);
        // A launch owns its thread, so two servers initialize at once instead
        // of one waiting out the other's whole handshake. The guard is taken
        // here and returned by a drop *inside* the thread, so it comes back on
        // every exit path — including a panic, which an explicit "finished"
        // call would never reach. A launch that dies must still let a shutdown
        // finish; a launch counter said the same thing until it turned out a
        // panicking launcher could leak it.
        // A thread that could not be created takes the closure, and the guard
        // inside it, with the error — so there is nothing to undo here. The
        // `Launch` guard inside has already marked the slot failed and woken
        // anyone waiting on it.
        let _ = std::thread::Builder::new()
            .name(format!("structural-launch-{index}"))
            .spawn(move || {
                let _guard = guard;
                entry.launch(index, launch);
            });
    }

    /// One launch, on its own thread: walk the slot's ladder until a
    /// candidate initializes, then publish it.
    ///
    /// The walk is **bounded by the ladder** — every candidate is tried at
    /// most once, in the plan's rank order, and the list ends. Nothing here
    /// retries, and no timer decides anything
    /// (`backlog/discovery/selection.md`).
    ///
    /// Two kinds of failure are kept apart, because only one of them is a
    /// statement about the server. [`Unstarted::Unavailable`] — the command
    /// would not spawn, or the handshake failed — is the candidate's, and
    /// the walk goes on to the next one. [`Unstarted::Refused`] is about
    /// this host, and the next candidate would meet the same wall, so the
    /// launch ends there.
    ///
    /// Spawning and `initialize` hold no lock; publication takes the
    /// workspace lock once, so no caller can observe a `Running` slot whose
    /// server is not yet in place. Failures go to the slot, which the
    /// `Launch` guard does on every exit path including a panic — so a
    /// waiter learns why rather than hanging.
    fn launch(&self, index: usize, launch: Launch) {
        if self.lifetime.stopped() {
            return launch.failed("the host is shutting down");
        }
        let slot = Arc::clone(launch.slot());
        for rung in 0..slot.rungs() {
            let adapter = slot.attempting(rung);
            match self.attempt(Arc::clone(&adapter)) {
                Ok((server, child)) => return self.publish(index, launch, server, child),
                // The candidate's own failure: retained, and the ladder
                // moves on. Retained even when the next one succeeds —
                // `descriptions` is where a successful fallback says what
                // it fell back from.
                Err(Unstarted::Unavailable(reason)) => slot.attempted(adapter.as_ref(), reason),
                Err(Unstarted::Refused(reason)) => return launch.failed(reason),
            }
        }
        launch.exhausted();
    }

    /// Start one candidate and bring it as far as a warmed index, or say
    /// why it could not get there.
    fn attempt(&self, adapter: Arc<dyn Adapter>) -> Result<(LspServer, Registration), Unstarted> {
        let spawning = match LspServer::spawn(adapter, &self.root) {
            Ok(spawning) => spawning,
            Err(error) => return Err(Unstarted::Unavailable(format!("{error:#}"))),
        };
        // Reachable by a shutdown from here on, though no workspace holds it
        // yet: this is the window in which a child would otherwise be
        // orphaned by a host that goes away mid-handshake.
        let Some(child) = self.lifetime.register(spawning.stopper()) else {
            // The host began stopping between the check above and this
            // registration, so nothing will stop this child but us. Dropping
            // the half-started server tears it down and reaps it.
            drop(spawning);
            return Err(Unstarted::Refused("the host is shutting down".to_string()));
        };
        let mut server = match spawning.initialize() {
            Ok(server) => server,
            // Availability is **successful initialization**, not an
            // executable on `PATH` (`docs/discovery.md`), so a handshake
            // that fails is exactly what the next candidate is for.
            Err(error) => return Err(Unstarted::Unavailable(format!("{error:#}"))),
        };
        // Populate the symbol index before anything can query it: several
        // servers answer `workspace/symbol` with nothing until a document
        // is open (docs/servers.md). On the launcher's own thread and
        // before the workspace lock is taken, so a tree walk and a file
        // read cannot delay an unrelated query.
        let _warmed = server.warm_index();
        Ok((server, child))
    }

    /// Put a started server in its slot, or stop it and say why it could
    /// not be published. Neither failure here is the candidate's, so
    /// neither walks the ladder.
    fn publish(&self, index: usize, launch: Launch, mut server: LspServer, child: Registration) {
        // The workspace lock, or nothing if it stays held past the bound. A
        // query holding this lock is unbounded in real time by design, so
        // waiting for it would make a shutdown wait with it.
        let Some(mut workspace) = self.lock_within(self.lifetime.publish_bound) else {
            drop(server);
            return launch.failed(format!(
                "this workspace was busy for {:?} while the server was starting, so it was \
                 stopped rather than left running",
                self.lifetime.publish_bound
            ));
        };
        if self.lifetime.stopped() {
            // Shutdown claimed the host while this server was starting. Drop it
            // rather than publish one nobody will look after — outside the
            // workspace lock, because dropping it reaps a child.
            drop(workspace);
            drop(server);
            return launch.failed("the host shut down while this server was starting");
        }
        child.published();
        server.set_registration(child);
        workspace.publish(index, server);
        launch.published();
    }
}
