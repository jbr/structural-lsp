//! One module per language: its candidates, its fixture, and its probes.
//!
//! Adding a language is a module here and, when the shared fixture does not
//! already carry the shape the probes need, a `Fixture` in
//! `src/fixtures.rs`. The harness in `main.rs` does not change.

pub mod bash;
pub mod cpp;
pub mod elm;
pub mod erlang;
pub mod gleam;
pub mod go;
pub mod python;
pub mod qml;
pub mod r;
pub mod ruby;
pub mod rust;
pub mod swift;
pub mod typescript;
