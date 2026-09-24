//! What a tool call is waiting on, for its progress heartbeat.
//!
//! A query legitimately waits as long as a language server needs — a cold
//! index can take minutes — and the client should hear that it is alive, and
//! why. The tool body runs on a blocking thread of its own (`tools::unblock`),
//! so the waits deep inside it report through a slot installed on that thread
//! rather than through every signature between the tool and the wait: a wait
//! names itself with [`waiting_on`], and the heartbeat reads the slot.
//!
//! Off such a thread — the command line, a test calling a tool body directly
//! — nothing is installed and [`waiting_on`] does nothing.

use std::cell::RefCell;
use std::sync::{Arc, Mutex, PoisonError};

/// One call's slot, shared between its blocking thread and its heartbeat.
#[derive(Clone, Debug, Default)]
pub struct Status(Arc<Mutex<Option<String>>>);

impl Status {
    /// What the call is waiting on right now, if anything has said.
    pub fn current(&self) -> Option<String> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn replace(&self, value: Option<String>) -> Option<String> {
        std::mem::replace(
            &mut self.0.lock().unwrap_or_else(PoisonError::into_inner),
            value,
        )
    }
}

thread_local! {
    static CURRENT: RefCell<Option<Status>> = const { RefCell::new(None) };
}

/// Make `status` this thread's slot until the returned guard drops.
pub fn install(status: Status) -> Installed {
    let previous = CURRENT.with(|current| current.replace(Some(status)));
    Installed(previous)
}

/// See [`install`].
#[must_use = "the slot is uninstalled when this drops"]
pub struct Installed(Option<Status>);

impl Drop for Installed {
    fn drop(&mut self) {
        let previous = self.0.take();
        CURRENT.with(|current| *current.borrow_mut() = previous);
    }
}

/// Say what this thread is about to wait on, until the returned guard drops
/// and the previous description is restored.
pub fn waiting_on(what: impl Into<String>) -> Waiting {
    let status = CURRENT.with(|current| current.borrow().clone());
    let previous = status
        .as_ref()
        .and_then(|status| status.replace(Some(what.into())));
    Waiting { status, previous }
}

/// See [`waiting_on`].
#[must_use = "the description is withdrawn when this drops"]
pub struct Waiting {
    status: Option<Status>,
    previous: Option<String>,
}

impl Drop for Waiting {
    fn drop(&mut self) {
        if let Some(status) = &self.status {
            status.replace(self.previous.take());
        }
    }
}
