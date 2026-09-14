//! Library surface for in-process tests: the same tools/state the binary
//! serves, without `mcplease::run`'s stdio ownership. Mirrors
//! semantic-edit-mcp's `lib.rs` pattern.

#![allow(dead_code)]

pub mod config;
pub mod lsp;
pub mod state;
pub mod tools;
