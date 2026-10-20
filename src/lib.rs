//! Library surface for in-process tests: the same tools/state the binary
//! serves, without the binary's stdio and argv. Mirrors semantic-edit-mcp's
//! `lib.rs` pattern.

pub mod clock;
pub mod config;
pub mod discovery;
pub mod fixtures;
pub mod host;
pub mod lsp;
pub mod operator;
pub mod progress;
pub mod session;
#[cfg(test)]
mod test_support;
pub mod tools;
pub mod workspace;
