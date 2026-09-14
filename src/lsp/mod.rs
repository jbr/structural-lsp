//! The LSP transport and the pure helpers around it.
//!
//! [`handle`] runs one language server on its own threads and exposes a
//! blocking [`handle::ServerHandle`]; [`session`] owns documents and speaks
//! rust-analyzer's dialect on top of it; [`adapter`] is where server-specific
//! knowledge lives. [`framing`], [`jsonrpc`] and [`handlers`] are the pure
//! kernel underneath; [`definition`], [`positions`] and [`shape`] are pure
//! helpers over query results and source text.

pub mod adapter;
pub mod definition;
pub mod framing;
pub mod handle;
pub mod handlers;
pub mod jsonrpc;
pub mod positions;
pub mod session;
pub mod shape;
