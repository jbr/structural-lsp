//! Filesystem observation: the event source every workspace gets, and the
//! thread that turns events into evidence maintenance and idle eviction.
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
//! nothing about a server's index (`docs/correctness.md`).
//!
//! The same thread runs the idle sweep: its park is bounded by the next
//! slot's expiry, and every wake — batch or park spent — ends with a sweep
//! of what has since expired. A lost watcher degrades the source to a
//! ticker: no refreshes, but the sweep — which needs only a clock — keeps
//! running, and each sweep retries the watch (`docs/observation.md`).
//!
//! The events themselves live in a [`Pending`] buffer both consumers
//! share: this thread, which coalesces them into batches for evidence,
//! and the query path, which drains them at the query boundary so a
//! query sees every change the watcher delivered before the query was
//! admitted (`docs/correctness.md`).

use crate::host::Entry;
use notify::Watcher;
use std::path::{Path, PathBuf};
use std::sync::Weak;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

mod pending;
#[cfg(test)]
mod synthetic;

use self::pending::{BATCH_CAP, Wait};
pub(crate) use pending::Pending;
#[cfg(test)]
pub(crate) use synthetic::{Driver, Synthetic};

/// Events arrive faster than evidence can be re-read. The source waits this
/// long after a first event to collect more before yielding a batch. Fixed
/// rather than configured: it is not a keepalive and never triggers a
/// launch, so it bounds evidence staleness at half a second and nothing
/// else.
const DEBOUNCE: Duration = Duration::from_millis(500);

/// One filesystem event, reduced to the three states evidence reacts to.
///
/// Access events are dropped at the source: they say a file was looked at,
/// not that the tree changed, and filesystem activity must not act as a
/// keepalive (`docs/observation.md`).
#[derive(Debug, Clone, PartialEq, Eq)]
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
///
/// An *empty* batch is not a batch: it is the park expiring — the sweep's
/// wake — and the observation loop runs only what is due on it, never a
/// refresh. A batch's emptiness is decided by the caller, not by the
/// source, which cannot tell a spent park from a quiet tree.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Batch {
    pub events: Vec<Event>,
}

/// Watching a root and yielding coalesced batches.
///
/// `None` from [`Source::next_batch`] means the source is finished — the
/// watcher was closed, or its channel broke — and the observation thread
/// stops on it. `park` bounds how long the source may block waiting for a
/// first event: `None` parks indefinitely, and a spent park yields an empty
/// batch so the caller can run what is due. The coalesce window stays
/// [`DEBOUNCE`] and is the source's own business.
pub(crate) trait Source: Send {
    /// Watch `root`, recursively. Called before any batch, and again after
    /// a loss — the retry is what makes a failed watch self-healing
    /// (`docs/observation.md`).
    fn watch(&mut self, root: &Path) -> anyhow::Result<()>;
    /// Block until at least one event is pending (or the park is spent),
    /// then coalesce.
    fn next_batch(&mut self, park: Option<Duration>) -> Option<Batch>;
    /// Whether the watch needs re-establishing. The source knows what its
    /// backend told it: a runtime error may mean events were dropped, or
    /// that the watch itself is gone — the two reduce to the same attempt,
    /// which is cheap when the watch is alive and healing when it is not
    /// (`docs/observation.md`).
    fn lost(&self) -> bool;
}

/// Where each workspace's events come from: a factory, not a source —
/// every workspace gets its own, handed the two buffers its events
/// land in — the observation thread's, which its coalesce drains
/// for evidence, and the query boundary's, which the query path
/// drains (`docs/correctness.md`) — paired with the handle that
/// closes them.
pub(crate) type SourceFactory =
    Box<dyn Fn(Arc<Pending>, Arc<Pending>) -> (Box<dyn Source>, Arc<dyn Stop>) + Send>;

/// The production factory: the platform's recursive watcher,
/// reduced (`docs/observation.md`).
pub(crate) fn default_source_factory() -> SourceFactory {
    Box::new(|evidence, watched| {
        let (source, stop) = Notify::new(evidence, watched);
        (Box::new(source), Arc::new(stop))
    })
}

/// Closing a source from outside the thread that owns it. The release path
/// ends a workspace's watching at once instead of at the tree's next event
/// (`docs/observation.md`).
pub(crate) trait Stop: Send + Sync {
    fn stop(&self);
}

/// The production source: the platform's recursive watcher, reduced.
///
/// The watcher is built on `watch`, not at construction — so a start
/// failure is a failed watch, which the sweep retries, rather than a
/// permanent state. After a loss the source degrades to a ticker:
/// nothing pushes, so every park spends itself and the sweep — which
/// needs only a clock — keeps running, while each sweep retries the
/// watch (`docs/observation.md`).
pub(crate) struct Notify {
    /// Shared with [`NotifyStop`], whose `stop` takes it — dropping
    /// the watcher drops the callback that pushes into the buffer.
    watcher: Arc<Mutex<Option<notify::RecommendedWatcher>>>,
    /// Set by the callback on a runtime error, and cleared by a watch that
    /// succeeds. A drop notice and a dead watch look the same from here;
    /// both are answered by re-establishing the watch.
    lost: Arc<AtomicBool>,
    /// The observation thread's buffer: the callback's events
    /// land in it, and the thread's coalesce drains it for
    /// evidence.
    evidence: Arc<Pending>,
    /// The query boundary's buffer: the same events land in it,
    /// and the query path drains it (`docs/correctness.md`).
    watched: Arc<Pending>,
}

/// The handle that closes a [`Notify`] from outside its thread.
pub(crate) struct NotifyStop {
    watcher: Arc<Mutex<Option<notify::RecommendedWatcher>>>,
    evidence: Arc<Pending>,
    watched: Arc<Pending>,
}

impl Notify {
    /// A source over two buffers: the watcher callback pushes
    /// every event into both, and `stop` closes them.
    pub(crate) fn new(evidence: Arc<Pending>, watched: Arc<Pending>) -> (Self, NotifyStop) {
        let watcher: Arc<Mutex<Option<notify::RecommendedWatcher>>> = Arc::new(Mutex::new(None));
        let lost = Arc::new(AtomicBool::new(false));
        (
            Self {
                watcher: Arc::clone(&watcher),
                lost: Arc::clone(&lost),
                evidence: Arc::clone(&evidence),
                watched: Arc::clone(&watched),
            },
            NotifyStop {
                watcher,
                evidence,
                watched,
            },
        )
    }

    /// Build the platform watcher, or report why it cannot be. Called from
    /// `watch` on first use and after every loss.
    fn build(&self) -> anyhow::Result<notify::RecommendedWatcher> {
        let evidence = Arc::clone(&self.evidence);
        let watched = Arc::clone(&self.watched);
        let lost = Arc::clone(&self.lost);
        notify::RecommendedWatcher::new(
            move |result: notify::Result<notify::Event>| match result {
                Ok(event) => {
                    // FSEvents coalesces history and marks "you may have
                    // missed some" with a rescan flag; either way the
                    // answer is the same reduction — re-read the tree.
                    if event.need_rescan() {
                        evidence.push(Event::Overflow);
                        watched.push(Event::Overflow);
                    } else {
                        for path in event.paths {
                            if let Some(event) = reduce(&path, event.kind) {
                                evidence.push(event.clone());
                                watched.push(event);
                            }
                        }
                    }
                }
                // A watcher error the reduction cannot interpret — which on
                // some platforms includes "events were dropped" — says the
                // same thing as overflow: the tree may have moved on
                // without us, so re-read it. It also says the watch itself
                // may be gone, so it is reported as a loss: the next sweep
                // re-establishes the watch either way.
                Err(_) => {
                    lost.store(true, Ordering::SeqCst);
                    evidence.push(Event::Overflow);
                    watched.push(Event::Overflow);
                }
            },
            notify::Config::default(),
        )
        .map_err(|error| anyhow::anyhow!("the platform watcher is unavailable: {error}"))
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
        let mut watcher = self.watcher.lock().unwrap_or_else(PoisonError::into_inner);
        if watcher.is_none() {
            *watcher = Some(self.build()?);
        }
        let result = watcher
            .as_mut()
            .expect("built just above")
            .watch(root, notify::RecursiveMode::Recursive);
        if result.is_ok() {
            self.lost.store(false, Ordering::SeqCst);
        }
        Ok(result?)
    }

    fn lost(&self) -> bool {
        self.lost.load(Ordering::SeqCst)
    }

    fn next_batch(&mut self, park: Option<Duration>) -> Option<Batch> {
        coalesce(&self.evidence, park)
    }
}

impl Stop for NotifyStop {
    fn stop(&self) {
        *self.watcher.lock().unwrap_or_else(PoisonError::into_inner) = None;
        self.evidence.close();
        self.watched.close();
    }
}

/// What the production source's [`Source::next_batch`] yields: a
/// first event, then every event that lands within the debounce
/// window, up to [`BATCH_CAP`]. `None` is the closed source.
fn coalesce(pending: &Pending, park: Option<Duration>) -> Option<Batch> {
    // Block for the first event: there is nothing to coalesce
    // against. A spent park is an empty batch — the sweep's wake,
    // not the tree changing.
    match pending.wait(park)? {
        Wait::Events => {}
        Wait::Spent => return Some(Batch { events: Vec::new() }),
    }
    let mut events = pending.drain();
    let deadline = Instant::now() + DEBOUNCE;
    while events.len() < BATCH_CAP
        && let Some(remaining) = deadline.checked_duration_since(Instant::now())
    {
        // Only more events extend the batch; a park spent or a
        // source closed within the window yields what has landed.
        match pending.wait(Some(remaining)) {
            Some(Wait::Events) => events.extend(pending.drain()),
            _ => break,
        }
    }
    Some(Batch { events })
}

/// The observation thread's body: watch, reduce, refresh, and — when an
/// agent is attached — warm the servers of languages the tree newly calls
/// for (`docs/observation.md`).
///
/// The thread holds only a [`Weak`] to the workspace's entry, and holds it
/// never across a wait — a released workspace is not kept alive by its own
/// watcher. The upgrade is re-checked before parking and after every wake:
/// release during a wait exits at the next wake, and release while a batch
/// is in flight exits after the refresh it caused (a refresh on a released
/// workspace is harmless — the map no longer names it, and slots append to
/// a workspace nobody answers).
///
/// Release ends the watching eagerly rather than at the tree's next event:
/// the entry's stop handle closes the source, `next_batch` ends it, and the
/// thread exits (`docs/observation.md`).
///
/// A watch failure is not death. The source degrades to a ticker — no
/// refreshes, but the sweep keeps running, because eviction needs only a
/// clock — and every sweep retries the watch, so a transient failure
/// self-heals and a persistent one costs one attempt per interval. The loss
/// and the recovery are each logged once; the retries in between stay
/// quiet (`docs/observation.md`).
pub(crate) fn observe(weak: &Weak<Entry>, mut source: Box<dyn Source>) {
    // The root and the interval only: the Arc is dropped before the loop, so
    // nothing below parks holding it — a released workspace is not kept
    // alive by its own watcher.
    let (root, idle) = match weak.upgrade() {
        Some(entry) => (entry.root.clone(), entry.idle_eviction),
        None => return,
    };
    let mut watching = match source.watch(&root) {
        Ok(()) => true,
        Err(error) => {
            eprintln!(
                "observation: {}: watch failed: {error:#}; evidence will not refresh \
                 until it recovers",
                root.display()
            );
            false
        }
    };
    loop {
        // The park bound: when the next slot the sweep could stop expires,
        // or nothing at all. While the watch is lost no event can arrive,
        // so the retry cadence is the sweep's — never parked longer than
        // one interval.
        let park = match weak.upgrade() {
            None => return,
            Some(entry) => {
                let park = entry.sweep_park();
                if watching {
                    park
                } else {
                    park.map(|park| park.min(idle)).or(Some(idle))
                }
            }
        };
        let Some(batch) = source.next_batch(park) else {
            return;
        };
        // Re-checked after: release during a wait exits here, and release
        // while a batch is in flight exits after the refresh it caused.
        let Some(entry) = weak.upgrade() else {
            return;
        };
        // An empty batch is the park spent, not the tree changing: the
        // reduction runs only on real events.
        if !batch.events.is_empty() {
            // The batch reduces to a refresh, and the refresh's new slots
            // are synced onto the entry in the same critical section, so
            // the indices it returns name the slots the demand path reads
            // without this lock (`Entry::sync_slots`). Taking `interested`
            // while holding the workspace lock cannot deadlock: nothing
            // takes `interested` and then waits for the workspace.
            let new_languages = {
                let mut workspace = entry.lock();
                let new = workspace.refresh();
                entry.sync_slots(workspace.slots());
                new
            };
            // A filesystem change is a weak signal: it warms a server only
            // when a language is new *and* an agent is attached. An
            // observed-only workspace updates evidence and launches nothing,
            // and plain activity never relaunches anything — filesystem
            // activity is not an unconditional keepalive
            // (`docs/observation.md`, the signal policy).
            if !new_languages.is_empty() && !entry.interested().is_empty() {
                entry.warm_indices(new_languages.iter().map(|&(_, index)| index).collect());
            }
        }
        // Every wake — batch or spent park — ends with the sweep. It stops
        // only what has actually expired, so a busy workspace finds nothing
        // and pays a lock acquisition for the look.
        let stopped = entry.sweep_due();
        drop(stopped);
        // Re-establish the watch at sweep cadence when the source reports
        // a loss or the last attempt failed. A drop notice and a dead watch
        // reduce to the same attempt: cheap when the watch is alive,
        // healing when it is not. The loss and the recovery are each
        // logged once; the retries in between stay quiet.
        if !watching || source.lost() {
            match source.watch(&root) {
                Ok(()) => {
                    if !watching {
                        eprintln!("observation: {}: watch recovered", root.display());
                    }
                    watching = true;
                }
                Err(_) => {
                    if watching {
                        eprintln!(
                            "observation: {}: watch lost; evidence will not refresh \
                             until it recovers",
                            root.display()
                        );
                        watching = false;
                    }
                }
            }
        }
    }
}
