//! Discovery: what a tree says about itself, before anything is started.
//!
//! The first of the four layers in `backlog/discovery/selection.md` —
//! evidence, catalogue, selection, acquisition. Everything here is a pure
//! function over the filesystem returning a value with its reasons; no
//! process is spawned and no lock is taken.

pub mod roots;
