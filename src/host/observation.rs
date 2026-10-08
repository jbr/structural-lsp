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
//! nothing about a server's index (`backlog/discovery/synchronization.md`).
//!
//! The same thread runs the idle sweep: its park is bounded by the next
//! slot's expiry, and every wake — batch or park spent — ends with a sweep
//! of what has since expired. A lost watcher degrades the source to a
//! ticker: no refreshes, but the sweep — which needs only a clock — keeps
//! running, and each sweep retries the watch (`docs/observation.md`).

use crate::host::Entry;
use notify::Watcher;
#[cfg(test)]
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Weak;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, channel};
use std::sync::{Arc, Mutex, PoisonError};
#[cfg(test)]
use std::sync::{Condvar, MutexGuard};
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
/// every workspace gets its own, paired with the handle that closes it.
pub(crate) type SourceFactory = Box<dyn Fn() -> (Box<dyn Source>, Arc<dyn Stop>) + Send>;

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
/// permanent state. After a loss the source degrades to a ticker: a
/// template sender keeps the event channel open, so `next_batch` parks and
/// yields spent-park batches, and the sweep keeps running with no refreshes
/// to do.
pub(crate) struct Notify {
    /// Shared with [`NotifyStop`], whose `stop` takes it — dropping the
    /// watcher drops the callback that holds a sender clone.
    watcher: Arc<Mutex<Option<notify::RecommendedWatcher>>>,
    /// Set by the callback on a runtime error, and cleared by a watch that
    /// succeeds. A drop notice and a dead watch look the same from here;
    /// both are answered by re-establishing the watch.
    lost: Arc<AtomicBool>,
    /// The template every watcher construction clones. Taken by `stop` so
    /// that stopping closes the channel: both senders gone, the observer
    /// thread's `next_batch` ends it. Held while lost, so the loss degrades
    /// to a ticker instead of an exit.
    sender: Arc<Mutex<Option<Sender<Event>>>>,
    events: Receiver<Event>,
}

/// The handle that closes a [`Notify`] from outside its thread.
pub(crate) struct NotifyStop {
    watcher: Arc<Mutex<Option<notify::RecommendedWatcher>>>,
    sender: Arc<Mutex<Option<Sender<Event>>>>,
}

impl Notify {
    /// A source with nothing built yet: `watch` constructs the watcher, and
    /// retries after a loss.
    pub(crate) fn new() -> (Self, NotifyStop) {
        let (sender, events) = channel();
        let watcher: Arc<Mutex<Option<notify::RecommendedWatcher>>> = Arc::new(Mutex::new(None));
        let sender = Arc::new(Mutex::new(Some(sender)));
        let lost = Arc::new(AtomicBool::new(false));
        (
            Self {
                watcher: Arc::clone(&watcher),
                sender: Arc::clone(&sender),
                lost: Arc::clone(&lost),
                events,
            },
            NotifyStop { watcher, sender },
        )
    }

    /// Build the platform watcher, or report why it cannot be. Called from
    /// `watch` on first use and after every loss.
    fn build(&self) -> anyhow::Result<notify::RecommendedWatcher> {
        let sender = self
            .sender
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .as_ref()
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("the source was stopped"))?;
        let lost = Arc::clone(&self.lost);
        notify::RecommendedWatcher::new(
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
                // without us, so re-read it. It also says the watch itself
                // may be gone, so it is reported as a loss: the next sweep
                // re-establishes the watch either way.
                Err(_) => {
                    lost.store(true, Ordering::SeqCst);
                    let _ = sender.send(Event::Overflow);
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
        // Block for the first event: there is nothing to coalesce against.
        // A spent park is an empty batch — the sweep's wake, not a refresh.
        let first = match park {
            None => self.events.recv().ok()?,
            Some(park) => match self.events.recv_timeout(park) {
                Ok(event) => event,
                Err(RecvTimeoutError::Timeout) => return Some(Batch { events: Vec::new() }),
                Err(RecvTimeoutError::Disconnected) => return None,
            },
        };
        let mut events = vec![first];
        let deadline = Instant::now() + DEBOUNCE;
        while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
            match self.events.recv_timeout(remaining) {
                Ok(event) => {
                    events.push(event);
                    if events.len() >= BATCH_CAP {
                        break;
                    }
                }
                Err(RecvTimeoutError::Timeout) => break,
                // The sender lives in the watcher and in the template; if
                // both are gone the source is finished and this batch is
                // the last.
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        Some(Batch { events })
    }
}

impl Stop for NotifyStop {
    fn stop(&self) {
        *self.watcher.lock().unwrap_or_else(PoisonError::into_inner) = None;
        *self.sender.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }
}

/// The test source: a test drives it, the observation thread drains it.
///
/// `next_batch` parks until a batch is pending, the park is spent, or the
/// source is closed. A test wakes the thread by pushing an empty batch —
/// the spent-park shape — and advances the clock before it, so eviction is
/// asserted against completed work, not timing.
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
    /// Set when `next_batch` has ended the source — the observer thread
    /// exits on it, so a test can await teardown.
    finished: bool,
    /// When set, `watch` fails: the test's way to produce a lost watcher
    /// and drive the degrade-to-ticker path.
    fail_watch: bool,
    /// Set by `Driver::lose`: the source reports a loss the way a backend
    /// error would, until a watch succeeds.
    lost: bool,
}

/// The handle a test keeps: push batches, close the source, read what was
/// watched, consumed, and finished. Also the [`Stop`] handle, since closing
/// the source is exactly what a release does.
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

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }
}

#[cfg(test)]
impl Driver {
    pub fn push(&self, batch: Batch) {
        let mut state = self.lock();
        state.pending.push_back(batch);
        drop(state);
        self.condvar.notify_all();
    }

    pub fn close(&self) {
        let mut state = self.lock();
        state.closed = true;
        drop(state);
        self.condvar.notify_all();
    }

    /// Make `watch` fail until cleared: the test's way to keep a
    /// re-establishment from succeeding.
    pub fn fail_watch(&self, fail: bool) {
        self.lock().fail_watch = fail;
    }

    /// Report a loss, the way a backend error would. Cleared by a watch
    /// that succeeds.
    pub fn lose(&self) {
        self.lock().lost = true;
    }

    /// The roots `watch` was called with, in order — the wiring under test
    /// is that the observer watches the workspace root, not a caller's cwd.
    pub fn watched(&self) -> Vec<PathBuf> {
        self.lock().watched.clone()
    }

    /// How many batches the source has handed out.
    pub fn consumed(&self) -> usize {
        self.lock().consumed
    }

    /// Whether the source has ended — the observer thread exits on it.
    pub fn finished(&self) -> bool {
        self.lock().finished
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap()
    }
}

#[cfg(test)]
impl Stop for Driver {
    fn stop(&self) {
        self.close()
    }
}

#[cfg(test)]
impl Source for Synthetic {
    fn watch(&mut self, root: &Path) -> anyhow::Result<()> {
        let mut state = self.lock();
        if state.fail_watch {
            return Err(anyhow::anyhow!("the test asked the watch to fail"));
        }
        state.lost = false;
        state.watched.push(root.to_path_buf());
        Ok(())
    }

    fn lost(&self) -> bool {
        self.lock().lost
    }

    fn next_batch(&mut self, park: Option<Duration>) -> Option<Batch> {
        let mut state = self.lock();
        // The park is a deadline like the native source's: a spent park is
        // an empty batch, whatever woke the condvar on the way.
        let deadline = park.map(|park| Instant::now() + park);
        loop {
            if let Some(batch) = state.pending.pop_front() {
                state.consumed += 1;
                return Some(batch);
            }
            if state.closed {
                state.finished = true;
                return None;
            }
            match deadline {
                Some(deadline) => match deadline.checked_duration_since(Instant::now()) {
                    None => return Some(Batch { events: Vec::new() }),
                    Some(remaining) => {
                        let (guard, _) = self
                            .condvar
                            .wait_timeout(state, remaining)
                            .expect("the mutex is not poisoned by a parked waiter");
                        state = guard;
                    }
                },
                None => {
                    state = self
                        .condvar
                        .wait(state)
                        .expect("the mutex is not poisoned by a parked waiter");
                }
            }
        }
    }
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
