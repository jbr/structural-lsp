//! What a language is, for the probe: the servers worth asking, the tree to
//! ask about, and the questions.
//!
//! The types are here and the answers are in `languages/`, one module per
//! language, so adding a language is a data change against a harness that
//! does not move.

use crate::report::Outcome;
use std::path::Path;

/// A server worth asking, with the argv the dataset recorded for it.
///
/// `builtin` marks a name `structural.toml` treats as an override of a
/// built-in adapter rather than a generic entry: the probe keeps the
/// built-in's readiness policy and capabilities, which is what that server
/// gets in production.
pub struct Candidate {
    pub name: &'static str,
    pub argv: &'static [&'static str],
    /// How to ask this server its version, when that is a different
    /// invocation than the server itself. `pyright-langserver --version`
    /// tries to open a connection and errors; the version lives on the
    /// sibling `pyright` binary.
    pub version_argv: &'static [&'static str],
    pub builtin: bool,
    /// `settings` for this candidate's `structural.toml` entry, as TOML
    /// written verbatim into the table.
    ///
    /// A server that serves nothing without client configuration is
    /// measured with the configuration, since the column is about what the
    /// server answers rather than about what a bare entry gets. What the
    /// setting has to be is itself a catalogue fact
    /// (`backlog/discovery/selection.md`).
    pub settings: &'static str,
    /// Extra lines for this candidate's `[servers.<name>]` table, as TOML
    /// written verbatim.
    ///
    /// For a protocol fact about the server that the entry has to state:
    /// ccls reserves a `$/progress` token at startup and never reports
    /// through it, so `progress_create_is_work = false` is what lets a
    /// readiness wait on it finish at all (`../../docs/servers.md`).
    pub extra_config: &'static str,
}

/// One question structural's tools need answered, and what a right answer
/// looks like. The name is what the scorecard prints.
pub struct Probe {
    pub name: &'static str,
    pub needs: &'static str,
    pub run: fn(&structural::session::Session) -> Outcome,
}

/// A file computed from the materialized scratch root: where it goes,
/// relative to the root, and how to render it once the root is known.
pub type GeneratedFile = (&'static str, fn(&Path) -> String);

/// A language's candidates, tree and questions.
pub struct Language {
    /// The name the scorecard prints and `structural.toml` configures.
    pub name: &'static str,
    /// The fixture in `src/fixtures.rs` this language is probed against.
    pub fixture: &'static str,
    /// File extensions for this language id, without the dot.
    ///
    /// `config.rs` knows extensions for eleven language ids and requires
    /// any other to spell them out, so a language it does not know is
    /// configurable but not guessable. Empty means "use what `config.rs`
    /// knows", which is right for the ids in that table and an error for
    /// the rest — the error names the syntax, so a wrong empty here is
    /// loud.
    pub extensions: &'static [&'static str],
    /// Files added to that fixture for the probe alone, as
    /// `(path, contents)`.
    ///
    /// The shared fixtures are asserted against by the smoke tests down to
    /// reference counts and line numbers, so a probe that needs one more
    /// call site adds it to its own scratch tree rather than to the tree
    /// four other tests measure. `rename.rs` writes its doc-link site the
    /// same way.
    pub extra_files: &'static [(&'static str, &'static str)],
    /// Files computed from the materialized root, as `(path, render)`.
    ///
    /// A compilation database names absolute paths, which a checked-in
    /// tree cannot hold: the fixture carries the template's facts and the
    /// probe renders them against the scratch root it measured.
    pub generated_files: &'static [GeneratedFile],
    pub candidates: &'static [Candidate],
    pub probes: &'static [Probe],
}

pub fn show(
    state: &structural::session::Session,
    symbol: Option<&str>,
    path: Option<&str>,
    anchor: Option<(&str, &str)>,
) -> anyhow::Result<String> {
    structural::tools::Definition {
        symbol: symbol.map(str::to_string),
        path: path.map(str::to_string),
        anchor: anchor.map(|(text, _)| text.to_string()),
        focus: anchor.map(|(_, focus)| focus.to_string()),
        view: None,
    }
    .run(state)
}

pub fn uses(state: &structural::session::Session, symbol: &str) -> anyhow::Result<String> {
    structural::tools::References {
        symbol: Some(symbol.to_string()),
        path: None,
        anchor: None,
        focus: None,
    }
    .run(state)
}

pub fn rename_preview(
    state: &structural::session::Session,
    symbol: &str,
    new_name: &str,
) -> anyhow::Result<String> {
    structural::tools::Rename {
        symbol: Some(symbol.to_string()),
        path: None,
        anchor: None,
        focus: None,
        new_name: Some(new_name.to_string()),
        mode: None,
        digest: None,
    }
    .run(state)
}
