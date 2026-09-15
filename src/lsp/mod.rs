//! The LSP transport and the pure helpers around it.
//!
//! [`handle`] runs one language server on its own threads and exposes a
//! blocking [`handle::ServerHandle`]; [`server`] owns documents and speaks LSP
//! on top of it for any server whose [`adapter::Adapter`] says how. [`framing`], [`jsonrpc`] and [`handlers`] are the pure
//! kernel underneath; [`anchor`], [`definition`], [`outline`], [`positions`], [`references`] and
//! [`shape`] are pure helpers over query results and source text.

pub mod adapter;
pub mod anchor;
pub mod definition;
pub mod framing;
pub mod handle;
pub mod handlers;
pub mod jsonrpc;
pub mod outline;
pub mod positions;
pub mod references;
pub mod server;
pub mod shape;
