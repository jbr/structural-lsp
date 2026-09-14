//! What the generic transport must not know about rust-analyzer.
//!
//! The full `Adapter` seam (command, languages, initialization options,
//! capability profile) lands with slice 2's work item 3; this module holds
//! the parts the `Session` migration already needs, so no rust-analyzer
//! string sits in `handle.rs` or `session.rs`'s readiness logic.

use super::ReadinessPolicy;
use serde_json::{Value, json};

/// The executable, as found on `PATH`.
pub const COMMAND: &str = "rust-analyzer";

/// The `experimental` client capabilities that make rust-analyzer send
/// `experimental/serverStatus`, which [`Quiescence`] observes. Without this
/// the policy never becomes ready.
pub fn experimental_capabilities() -> Value {
    json!({ "serverStatusNotification": true })
}

/// Ready only once rust-analyzer's latest `experimental/serverStatus` says
/// `quiescent: true`. Composed with standard progress by the transport, so a
/// quiescent status while a progress token is still open does not count.
///
/// Before the first status arrives the server is not ready: a cold start
/// blocks until rust-analyzer has finished its initial load, which is the
/// point. A later `quiescent: false` (re-indexing after an edit, a
/// `cargo check`) makes it not ready again until the next `true`.
#[derive(Debug, Default)]
pub struct Quiescence {
    quiescent: Option<bool>,
}

impl ReadinessPolicy for Quiescence {
    fn observe(&mut self, method: &str, params: Option<&Value>) {
        if method != "experimental/serverStatus" {
            return;
        }
        if let Some(quiescent) = params
            .and_then(|p| p.get("quiescent"))
            .and_then(Value::as_bool)
        {
            self.quiescent = Some(quiescent);
        }
    }

    fn is_ready(&self) -> bool {
        self.quiescent == Some(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(quiescent: bool) -> Value {
        json!({ "health": "ok", "quiescent": quiescent })
    }

    #[test]
    fn not_ready_until_a_quiescent_status_arrives() {
        let mut policy = Quiescence::default();
        assert!(
            !policy.is_ready(),
            "no status yet: a cold start is not ready"
        );
        policy.observe("$/progress", Some(&json!({ "token": 1 })));
        assert!(!policy.is_ready(), "other notifications say nothing");
        policy.observe("experimental/serverStatus", Some(&status(true)));
        assert!(policy.is_ready());
    }

    #[test]
    fn follows_the_latest_status() {
        let mut policy = Quiescence::default();
        policy.observe("experimental/serverStatus", Some(&status(true)));
        policy.observe("experimental/serverStatus", Some(&status(false)));
        assert!(
            !policy.is_ready(),
            "a later false supersedes the earlier true"
        );
        policy.observe("experimental/serverStatus", Some(&status(true)));
        assert!(policy.is_ready());
        // A status without the flag changes nothing.
        policy.observe(
            "experimental/serverStatus",
            Some(&json!({ "health": "warning" })),
        );
        assert!(policy.is_ready());
    }
}
