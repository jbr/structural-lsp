//! Per-server readiness policies: the seam where a server's own readiness
//! signal joins the transport's standard progress tracking.
//!
//! The transport ([`crate::lsp::handle`]) tracks *standard* readiness itself:
//! the set of `window/workDoneProgress/create` / `$/progress` tokens the
//! server has opened and not ended ("standard idle" = the set is empty). A
//! policy may add a stronger, server-specific condition; the transport
//! composes readiness as
//!
//! ```text
//! ready = standard_idle && policy.is_ready()
//! ```
//!
//! so a policy **cannot** override active standard progress: while any token
//! is open the connection is not ready, whatever the policy says. A policy
//! that wants a server-specific signal observes the notification stream
//! through [`ReadinessPolicy::observe`] and keeps its own protocol state —
//! it never rescans the event log, which is capped and diagnostics-only.
//!
//! Server-specific method knowledge belongs in a policy, not in the
//! transport: rust-analyzer's `experimental/serverStatus` quiescence policy
//! lands here during the Session migration (slice 2, work item 3's Adapter
//! seam). The generic transport names no experimental method.

use serde_json::Value;

/// A server's readiness policy beyond standard progress tracking.
///
/// All methods have defaults; [`StandardIdle`] is the "standard progress
/// only" policy. The transport calls `observe` for every notification the
/// server sends, in arrival order, before deciding readiness; `is_ready` is
/// consulted with the AND of standard idle described on the module.
pub trait ReadinessPolicy: Send {
    /// Fold one server notification into the policy's protocol state.
    /// Called on the owner thread for every notification, in order.
    fn observe(&mut self, _method: &str, _params: Option<&Value>) {}

    /// Whether the policy's own (server-specific) readiness condition holds.
    /// Consulted only after standard progress is idle; returning `false`
    /// keeps the connection not-ready, returning `true` never overrides
    /// active standard progress.
    fn is_ready(&self) -> bool {
        true
    }
}

/// The default policy: standard progress tracking only. Adds no condition of
/// its own, so readiness is exactly "no outstanding work" (an empty progress
/// token set) — the right default for servers that send no readiness signal
/// beyond `$/progress` (e.g. pyright, per the spike).
pub struct StandardIdle;

impl ReadinessPolicy for StandardIdle {}
