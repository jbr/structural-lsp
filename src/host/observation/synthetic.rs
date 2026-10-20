//! The test source: a [`Source`] a test drives by hand, over the same
//! two buffers a real watcher's callback writes (`docs/observation.md`).

use super::pending::{Pending, Wait};
use super::{Batch, Source, Stop};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

/// The test source: a test drives it, the observation thread drains
/// the evidence buffer, and the query path drains the watched one —
/// the two buffers a real watcher's callback writes.
///
/// `next_batch` parks until events are pending, the park is spent, or
/// the source is closed, and then hands out everything that has
/// landed: no debounce, so what a test pushed is what coalesces,
/// immediately and whole. Two pushes a wake apart are two handouts;
/// two pushes one wake sees are one — a test that means two sequences
/// them. A test advances the clock before a park, so eviction is
/// asserted against completed work, not timing.
pub(crate) struct Synthetic {
    state: Arc<Mutex<State>>,
    /// The observation thread's buffer, the one its coalesce
    /// drains — the same one a [`Notify`](super::Notify) source's watcher writes.
    evidence: Arc<Pending>,
}

#[derive(Default)]
struct State {
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

/// The handle a test keeps: push events, close the source, read what was
/// watched, consumed, and finished. Also the [`Stop`] handle, since closing
/// the source is exactly what a release does.
#[derive(Clone)]
pub(crate) struct Driver {
    state: Arc<Mutex<State>>,
    evidence: Arc<Pending>,
    watched: Arc<Pending>,
}

impl Synthetic {
    pub(crate) fn new(evidence: Arc<Pending>, watched: Arc<Pending>) -> (Self, Driver) {
        let state = Arc::new(Mutex::new(State::default()));
        (
            Self {
                state: Arc::clone(&state),
                evidence: Arc::clone(&evidence),
            },
            Driver {
                state,
                evidence,
                watched,
            },
        )
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Driver {
    pub fn push(&self, batch: Batch) {
        if batch.events.is_empty() {
            // An empty batch is not a batch: it is the park
            // expiring, the sweep's wake (`docs/observation.md`).
            self.evidence.nudge();
        } else {
            for event in batch.events {
                self.evidence.push(event.clone());
                self.watched.push(event);
            }
        }
    }

    pub fn close(&self) {
        self.lock().closed = true;
        self.evidence.close();
        self.watched.close();
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
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Stop for Driver {
    fn stop(&self) {
        self.close()
    }
}

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
        // However the source ends — closed at entry, or closed under a
        // park — the observer thread is ending on this handout.
        let batch = if self.lock().closed {
            None
        } else {
            match self.evidence.wait(park) {
                Some(Wait::Events) => Some(Batch {
                    events: self.evidence.drain(),
                }),
                Some(Wait::Spent) => Some(Batch { events: Vec::new() }),
                None => None,
            }
        };
        match &batch {
            Some(_) => self.lock().consumed += 1,
            None => self.lock().finished = true,
        }
        batch
    }
}
