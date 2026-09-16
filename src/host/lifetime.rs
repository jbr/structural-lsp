//! Admission and process reachability, independent of workspace/query locks.
use crate::lsp::handle::Stopper;
use anyhow::{Result, bail};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;
use swansong::{Guard, Swansong};

#[derive(Clone)]
pub(crate) struct Lifetime {
    pub(super) song: Swansong,
    stopped: Arc<AtomicBool>,
    state: Arc<Mutex<State>>,
    pub(super) publish_bound: Duration,
}

#[derive(Default)]
struct State {
    forced: bool,
    next: u64,
    children: BTreeMap<u64, Child>,
}
struct Child {
    stopper: Stopper,
    starting: bool,
}

impl Lifetime {
    pub(crate) fn new() -> Self {
        Self::with_publish_bound(super::DEFAULT_PUBLISH_BOUND)
    }
    pub(crate) fn with_publish_bound(publish_bound: Duration) -> Self {
        Self {
            song: Swansong::new(),
            stopped: Arc::new(AtomicBool::new(false)),
            state: Arc::new(Mutex::new(State::default())),
            publish_bound,
        }
    }
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
    pub(crate) fn flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.stopped)
    }
    pub(crate) fn stopped(&self) -> bool {
        self.stopped.load(Ordering::SeqCst)
    }

    /// Register before election or tool work. Closing admission takes this
    /// same lock, so completion cannot miss a participant about to start.
    pub(crate) fn admit(&self) -> Result<Guard> {
        let _state = self.lock();
        if self.stopped() {
            bail!("the host is shutting down");
        }
        Ok(self.song.guard())
    }

    pub(crate) fn stop(&self) {
        let state = self.lock();
        self.stopped.store(true, Ordering::SeqCst);
        drop(self.song.shut_down());
        for child in state.children.values().filter(|child| child.starting) {
            child.stopper.stop();
        }
    }

    pub(crate) fn force(&self) {
        let mut state = self.lock();
        self.stopped.store(true, Ordering::SeqCst);
        state.forced = true;
        drop(self.song.shut_down());
        for child in state.children.values() {
            child.stopper.force();
        }
    }

    pub(crate) fn register(&self, stopper: Stopper) -> Option<Registration> {
        let mut state = self.lock();
        if self.stopped() {
            if state.forced {
                stopper.force();
            } else {
                stopper.stop();
            }
            return None;
        }
        let id = state.next;
        state.next += 1;
        state.children.insert(
            id,
            Child {
                stopper,
                starting: true,
            },
        );
        Some(Registration {
            lifetime: self.clone(),
            id,
        })
    }
}

/// Kept by the launch, then the published server, through process teardown.
/// A stopped old instance cannot remove a replacement instance's capability.
pub(crate) struct Registration {
    lifetime: Lifetime,
    id: u64,
}
impl Registration {
    pub(crate) fn published(&self) {
        if let Some(child) = self.lifetime.lock().children.get_mut(&self.id) {
            child.starting = false;
        }
    }
}
impl Drop for Registration {
    fn drop(&mut self) {
        self.lifetime.lock().children.remove(&self.id);
    }
}
