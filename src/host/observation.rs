//! Filesystem observation: the event source every workspace gets, and the
//! thread that turns events into evidence maintenance.
//!
//! [`Source`] is a seam because the workspace refresh it drives is
//! testable against a source the test drives — no kernel watcher in the
//! loop (`docs/observation.md`). The native backend behind the default is
//! the `notify` crate.
//!
//! What a batch *means* is deliberately almost nothing: every batch —
//! including an overflow — reduces to "re-read the tree". A recursive walk
//! under the [`FILE_BUDGET`](crate::discovery::evidence::FILE_BUDGET) cap is
//! cheap enough that replaying events into the evidence would buy nothing,
//! and a full rescan is reconciliation by construction: lost events cannot
//! corrupt what was never trusted. The batch's paths are carried for the
//! day something selective is worth proving, not used today.
//!
//! Nothing here claims freshness. A refresh lays out slots for languages
//! the tree newly calls for and grows the entry's copy of the slot list;
//! whether a new language's server warms is the signal policy — a
//! filesystem change is a weak signal, so it warms only for a workspace
//! with an attached agent, and an observed-only workspace updates evidence
//! and launches nothing (`docs/observation.md`). A watcher existing says
//! nothing about a server's index (`backlog/discovery/synchronization.md`).

use crate::host::Entry;
use anyhow::bail;
use notify::Watcher;
#[cfg(test)]
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Weak;
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
#[cfg(test)]
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// Events arrive faster than evidence can be re-read. The source waits this
/// long after a first event to collect more before yielding a batch. Fixed
/// rather than configured: it is not a keepalive and never triggers a
/// launch, so it bounds evidence staleness at half a second and nothing
/// else.
const DEBOUNCE: Duration = Duration::from_millis(500);

/// A batch stops coalescing above this size; what is left coalesces into
/// the next one. Bounding it keeps a flood from growing memory without
/// bound, and since the reduction is a full rescan, truncation costs
/// nothing correct.
const BATCH_CAP: usize = 4096;

/// One filesystem event, reduced to the three states evidence reacts to.
///
/// Access events are dropped at the source: they say a file was looked at,
/// not that the tree changed, and filesystem activity must not act as a
/// keepalive (`backlog/discovery/README.md`).
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Event {
    Created {
        path: PathBuf,
    },
    Changed {
        path: PathBuf,
    },
    Removed {
        path: PathBuf,
    },
    /// The backend lost events — an error it could not interpret, or a
    /// platform notice that some may have been dropped. Carried as its own
    /// event so the reduction cannot distinguish lost from received: both
    /// say "re-read the tree".
    Overflow,
}

/// What the source coalesced into one reduction.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Batch {
    pub events: Vec<Event>,
}

/// Watching a root and yielding coalesced batches.
///
/// `None` from [`Source::next_batch`] means the source is finished — the
/// watcher was closed, or its channel broke — and the observation thread
/// stops on it. `debounce` is the coalesce window; a synthetic source is
/// free to ignore it, because the test controls coalescing by what it
/// pushes.
pub(crate) trait Source: Send {
    /// Watch `root`, recursively. Called once, before any batch.
    fn watch(&mut self, root: &Path) -> anyhow::Result<()>;
    /// Block until at least one event is pending, then coalesce.
    fn next_batch(&mut self, debounce: Duration) -> Option<Batch>;
}

/// The production source: the platform's recursive watcher, reduced.
pub(crate) struct Notify {
    /// `None` when the backend could not be created — fd exhaustion, or a
    /// platform without a watcher. The construction runs in the
    /// workspace-creation path, so it must not panic there: a source
    /// without a watcher errors on `watch` and yields no batches, which is
    /// the behaviour the workspace had before this unit.
    watcher: Option<notify::RecommendedWatcher>,
    events: Receiver<Event>,
}

impl Notify {
    pub(crate) fn new() -> Self {
        let (sender, events) = channel();
        let watcher = notify::RecommendedWatcher::new(
            move |result: notify::Result<notify::Event>| match result {
                Ok(event) => {
                    // FSEvents coalesces history and marks "you may have
                    // missed some" with a rescan flag; either way the
                    // answer is the same reduction — re-read the tree.
                    if event.need_rescan() {
                        let _ = sender.send(Event::Overflow);
                    } else {
                        for path in event.paths {
                            if let Some(event) = reduce(&path, event.kind) {
                                let _ = sender.send(event);
                            }
                        }
                    }
                }
                // A watcher error the reduction cannot interpret — which on
                // some platforms includes "events were dropped" — says the
                // same thing as overflow: the tree may have moved on
                // without us, so re-read it.
                Err(_) => {
                    let _ = sender.send(Event::Overflow);
                }
            },
            notify::Config::default(),
        );
        // On failure the callback holding the sender is dropped, so the
        // channel closes: `next_batch` ends the source without a batch.
        Self {
            watcher: watcher.ok(),
            events,
        }
    }
}

pub(crate) fn reduce(path: &Path, kind: notify::EventKind) -> Option<Event> {
    use notify::EventKind as Kind;
    match kind {
        Kind::Create(_) => Some(Event::Created {
            path: path.to_path_buf(),
        }),
        Kind::Modify(_) => Some(Event::Changed {
            path: path.to_path_buf(),
        }),
        Kind::Remove(_) => Some(Event::Removed {
            path: path.to_path_buf(),
        }),
        // Access says a file was looked at, not that the tree changed.
        // Any is the unset kind; Other carries kinds this reduction cannot
        // interpret. Both say nothing.
        Kind::Any | Kind::Access(_) | Kind::Other => None,
    }
}

impl Source for Notify {
    fn watch(&mut self, root: &Path) -> anyhow::Result<()> {
        let Some(watcher) = self.watcher.as_mut() else {
            bail!("the platform watcher is unavailable");
        };
        Ok(watcher.watch(root, notify::RecursiveMode::Recursive)?)
    }

    fn next_batch(&mut self, debounce: Duration) -> Option<Batch> {
        // Block for the first event: there is nothing to coalesce against.
        let first = self.events.recv().ok()?;
        let mut events = vec![first];
        let deadline = Instant::now() + debounce;
        while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
            match self.events.recv_timeout(remaining) {
                Ok(event) => {
                    events.push(event);
                    if events.len() >= BATCH_CAP {
                        break;
                    }
                }
                Err(RecvTimeoutError::Timeout) => break,
                // The sender lives in the watcher; if it is gone the source
                // is finished and this batch is the last.
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        Some(Batch { events })
    }
}

/// The test source: a test drives it, the observation thread drains it.
///
/// `next_batch` parks until a batch is pending, so a test waits on a
/// condition it controls rather than on the thread's timing, and ignores
/// `debounce` — what coalesces is what the test pushed.
#[cfg(test)]
pub(crate) struct Synthetic {
    state: Arc<Mutex<State>>,
    /// Signalled on `push` and `close`. Out of [`State`] so a parked
    /// `wait` can move the guard — a condvar inside the mutex's payload
    /// borrows through it, which cannot compile.
    condvar: Arc<Condvar>,
}

#[cfg(test)]
#[derive(Default)]
struct State {
    pending: VecDeque<Batch>,
    closed: bool,
    watched: Vec<PathBuf>,
    /// Batches handed out, for tests that assert what was consumed.
    consumed: usize,
}

/// The handle a test keeps: push batches, close the source, read what was
/// watched and consumed.
#[derive(Clone)]
#[cfg(test)]
pub(crate) struct Driver {
    state: Arc<Mutex<State>>,
    condvar: Arc<Condvar>,
}

#[cfg(test)]
impl Synthetic {
    pub(crate) fn new() -> (Self, Driver) {
        let state = Arc::new(Mutex::new(State::default()));
        let condvar = Arc::new(Condvar::new());
        (
            Self {
                state: Arc::clone(&state),
                condvar: Arc::clone(&condvar),
            },
            Driver { state, condvar },
        )
    }
}

#[cfg(test)]
impl Driver {
    pub fn push(&self, batch: Batch) {
        let mut state = self.state.lock().unwrap();
        state.pending.push_back(batch);
        drop(state);
        self.condvar.notify_all();
    }

    pub fn close(&self) {
        let mut state = self.state.lock().unwrap();
        state.closed = true;
        drop(state);
        self.condvar.notify_all();
    }

    /// The roots `watch` was called with, in order — the wiring under test
    /// is that the observer watches the workspace root, not a caller's cwd.
    pub fn watched(&self) -> Vec<PathBuf> {
        self.state.lock().unwrap().watched.clone()
    }

    /// How many batches the source has handed out.
    pub fn consumed(&self) -> usize {
        self.state.lock().unwrap().consumed
    }
}

#[cfg(test)]
impl Source for Synthetic {
    fn watch(&mut self, root: &Path) -> anyhow::Result<()> {
        self.state.lock().unwrap().watched.push(root.to_path_buf());
        Ok(())
    }

    fn next_batch(&mut self, _debounce: Duration) -> Option<Batch> {
        let mut state = self.state.lock().unwrap();
        loop {
            if let Some(batch) = state.pending.pop_front() {
                state.consumed += 1;
                return Some(batch);
            }
            if state.closed {
                return None;
            }
            state = self
                .condvar
                .wait(state)
                .expect("the mutex is not poisoned by a parked waiter");
        }
    }
}

/// The observation thread's body: watch, reduce, refresh, and — when an
/// agent is attached — warm the servers of languages the tree newly calls
/// for (`docs/observation.md`).
///
/// The thread holds only a [`Weak`] to the entry, and holds it never across
/// a wait — a released workspace is not kept alive by its own watcher. The
/// upgrade is re-checked after every batch: release during a wait exits at
/// the next batch, and release while a batch is in flight exits after it
/// (a refresh on a released workspace is harmless — the map no longer names
/// it, and slots append to a workspace nobody answers).
///
/// Watch errors end observation quietly: a workspace whose watcher failed
/// behaves as it did before this unit — evidence goes stale — and a stderr
/// line is the one place a person can learn why.
///
/// A released workspace whose tree then goes quiet leaves this thread
/// parked with a live watcher until the tree's next event — nothing wakes
/// it to notice the release. Closing released watchers eagerly is the
/// residency part of this unit, not this reduction
/// (`backlog/discovery/observation.md`).
pub(crate) fn observe(weak: &Weak<Entry>, mut source: Box<dyn Source>) {
    // The root only: the Arc is dropped inside the `map`, so nothing below
    // parks holding it — a released workspace is not kept alive by its own
    // watcher.
    let Some(root) = weak.upgrade().map(|entry| entry.root.clone()) else {
        return;
    };
    if let Err(error) = source.watch(&root) {
        eprintln!("observation: {}: {error:#}", root.display());
        return;
    }
    loop {
        // Checked before parking; the upgrade's Arc is a temporary here and
        // dies with the check.
        if weak.upgrade().is_none() {
            return;
        }
        let Some(_batch) = source.next_batch(DEBOUNCE) else {
            return;
        };
        // Re-checked after: release during a wait exits here, and release
        // while a batch is in flight exits after the refresh it caused.
        let Some(entry) = weak.upgrade() else {
            return;
        };
        // The batch reduces to a refresh, and the refresh's new slots are synced
        // onto the entry in the same critical section, so the indices it
        // returns name the slots the demand path reads without this lock
        // (`Entry::sync_slots`). Taking `interested` while holding the
        // workspace lock cannot deadlock: nothing takes `interested` and
        // then waits for the workspace.
        let new_languages = {
            let mut workspace = entry.lock();
            let new = workspace.refresh();
            entry.sync_slots(workspace.slots());
            new
        };
        // A filesystem change is a weak signal: it warms a server only when
        // a language is new *and* an agent is attached. An observed-only
        // workspace updates evidence and launches nothing, and plain
        // activity never relaunches anything — filesystem activity is not
        // an unconditional keepalive (`docs/observation.md`, the signal
        // policy).

        if !new_languages.is_empty() && !entry.interested().is_empty() {
            entry.warm_indices(new_languages.iter().map(|&(_, index)| index).collect());
        }
    }
}
