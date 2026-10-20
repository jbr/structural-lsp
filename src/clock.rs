//! Where "now" comes from for residency.
//!
//! Eviction is a resource-expiry timer, not a semantic one: it decides when
//! a server process is released, never when an answer is ready — the
//! distinction `docs/correctness.md` draws. The clock is injectable so
//! tests advance time rather than sleeping through intervals
//! (`docs/observation.md`).
//!
//! Every reader takes the clock as an `Arc` clone of the same instance, so
//! a test's `advance` moves the past along with the present: timestamps
//! slots already carry were read from this clock, and the sweep compares
//! them against it too.

use std::time::Instant;

/// The source of "now" for slot creation, the touch a strong signal gives a
/// slot, and the sweep that expires one.
pub trait Clock: Send + Sync {
    fn now(&self) -> Instant;
}

/// The real clock.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }
}

/// A clock a test controls: `advance` moves every reader's now at once.
#[cfg(test)]
pub struct TestClock {
    now: std::sync::Mutex<Instant>,
}

#[cfg(test)]
impl TestClock {
    /// A clock standing at real "now", shared as the arc every reader takes.
    pub fn shared() -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            now: std::sync::Mutex::new(Instant::now()),
        })
    }

    /// Move the clock forward. Every reader — the sweep, the park
    /// computation, timestamps slots already carry — sees the new now.
    pub fn advance(&self, by: std::time::Duration) {
        let mut now = self
            .now
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *now = now
            .checked_add(by)
            .expect("the test clock never advances past the platform's range");
    }
}

#[cfg(test)]
impl Clock for TestClock {
    fn now(&self) -> Instant {
        *self
            .now
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}
