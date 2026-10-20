//! The event buffers: what a workspace's watcher has seen
//! and no consumer has reduced yet.
//!
//! The watcher's events land in two buffers per workspace
//! (`docs/correctness.md`, `docs/observation.md`): the
//! observation thread's, which its coalesce drains into batches
//! for evidence, and the query boundary's, which the query path
//! drains so a query sees every change the watcher delivered
//! before the query was admitted. One callback fills both, so
//! neither consumer takes an event from the other. Coalescing is
//! the observation thread's business alone; a drain takes what is
//! there, immediately, and never waits on a debounce.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex, PoisonError};
use std::time::Duration;

use super::Event;

/// How many events may wait unreduced. The bound is the one the
/// observation thread's batches carry: past it, the buffer stops
/// growing and holds an overflow marker in place of the events it
/// cannot hold — the same reduction a lost event always had,
/// "re-read the tree" (`docs/observation.md`).
pub(super) const BATCH_CAP: usize = 4096;

/// The events awaiting reduction, and the wakes a test asked for.
#[derive(Default)]
struct Queue {
    events: VecDeque<Event>,
    /// Wakes with nothing to show, counted: each is one park's
    /// expiry (`nudge`).
    nudges: usize,
}

/// Filesystem events the watcher has seen and no consumer has
/// reduced yet.
pub(crate) struct Pending {
    queue: Mutex<Queue>,
    signal: Condvar,
    closed: AtomicBool,
}

/// What a park on [`Pending`] ended in.
pub(super) enum Wait {
    /// At least one event is pending; [`Pending::drain`] holds it.
    Events,
    /// The park spent itself with nothing to show: the sweep's
    /// wake, not the tree changing.
    Spent,
}

impl Pending {
    pub(crate) fn new() -> Self {
        Self {
            queue: Mutex::new(Queue::default()),
            signal: Condvar::new(),
            closed: AtomicBool::new(false),
        }
    }

    /// The watcher callback's only write. Wakes every park.
    pub(super) fn push(&self, event: Event) {
        let mut queue = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
        if queue.events.len() < BATCH_CAP {
            queue.events.push_back(event);
        } else if !queue.events.contains(&Event::Overflow) {
            // Full: the events past the cap are lost, and the
            // marker says so rather than pretending the buffer is
            // the whole truth.
            queue.events.push_back(Event::Overflow);
        }
        drop(queue);
        self.signal.notify_all();
    }

    /// Everything that arrived since the last drain, in arrival
    /// order. This is the query boundary: what a query may treat
    /// as delivered to the servers that would answer it
    /// (`docs/correctness.md`).
    pub(crate) fn drain(&self) -> Vec<Event> {
        let mut queue = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
        queue.events.drain(..).collect()
    }

    /// Park until an event is pending or the park is spent. `None`
    /// is the closed source: the observer thread ends on it.
    pub(super) fn wait(&self, park: Option<Duration>) -> Option<Wait> {
        let mut queue = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
        loop {
            if self.closed.load(Ordering::SeqCst) {
                return None;
            }
            if !queue.events.is_empty() {
                return Some(Wait::Events);
            }
            // A wake with nothing to show is the park expiring.
            // Counted, so a test's pokes are one handout each.
            if queue.nudges > 0 {
                queue.nudges -= 1;
                return Some(Wait::Spent);
            }
            match park {
                // An indefinite park is woken only by a push or a
                // close.
                None => {
                    queue = self
                        .signal
                        .wait(queue)
                        .unwrap_or_else(PoisonError::into_inner)
                }
                Some(park) => {
                    let (guard, spent) = self
                        .signal
                        .wait_timeout(queue, park)
                        .unwrap_or_else(PoisonError::into_inner);
                    queue = guard;
                    if spent.timed_out() && queue.events.is_empty() {
                        return Some(Wait::Spent);
                    }
                }
            }
        }
    }

    /// Wake every park with nothing to show — the park expiring,
    /// which is what a test's empty batch says (`docs/observation.md`).
    /// A real backend wakes a park by pushing an event or by the
    /// park spending itself, so only the test source nudges.
    #[cfg(test)]
    pub(super) fn nudge(&self) {
        let mut queue = self.queue.lock().unwrap_or_else(PoisonError::into_inner);
        queue.nudges += 1;
        drop(queue);
        self.signal.notify_all();
    }

    /// End the source: no further events will arrive, and every
    /// park ends. The buffer stays drainable, so a query that raced
    /// the close still sees what landed.
    pub(super) fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        self.signal.notify_all();
    }
}
