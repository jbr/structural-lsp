//! Reusable LSP transport kernel, promoted from the spike's `spike_transport.rs`.
//!
//! What lives here serves the two actual callers — the MCP `show` slice
//! ([`super::session`]) and the preserved spike harness (`crate::spike`) — and
//! nothing more: there is no generic server registry and no lifecycle
//! abstraction beyond what those callers need.
//!
//! Concurrency honesty: the client can hold several requests outstanding and
//! routes responses by id (`send_request`/`wait_response`), which is what LSP's
//! overlapping calls require; the MCP slice itself issues its requests
//! strictly sequentially on mcplease's single dispatch thread. Full sans-IO
//! protocol evolution is deferred, not ruled unnecessary.

pub mod adapter;
pub mod client;
pub mod definition;
pub mod framing;
pub mod handle;
pub mod handlers;
pub mod jsonrpc;
pub mod positions;
pub mod session;
pub mod shape;
