//! The keys a project writes: [`Config`] and [`ServerEntry`], deserialized
//! from `structural.toml` or from an embedder's own file, and the source of
//! the JSON schema committed as `schemas/config.json` and of any schema an
//! embedder publishes with them (`docs/servers.md`).
//!
//! On these types `///` is hover text for someone editing the file: it
//! describes the key, its values and its default in terms of the file, and
//! names no repository path. It does not name structural either: embedded,
//! this is another product's language-server feature, under that product's
//! name, and the same text has to read right in both. Notes for implementers
//! are `//`.

use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;
use std::time::Duration;

/// How long a server may sit without a strong signal — a bind or a query
/// routing to it — before the idle sweep releases it, observation going on
/// regardless (`docs/observation.md`). One interval for every server, by
/// the operator's decision, not a table of them; `idle-eviction-secs`
/// overrides it per workspace, and its hover text states this value, which
/// a test holds the two together on.
pub const DEFAULT_IDLE_EVICTION: Duration = Duration::from_secs(30 * 60);

/// Which language servers run for this project, and how. Every key is
/// optional. With none, each language the project contains gets the
/// best-ranked known server that is installed. An unknown key fails the
/// whole configuration, and then no server starts.
// The override layer over the server catalogue (docs/discovery.md): what
// starts is what selection plans from the tree's evidence, and this names,
// adds or disables catalogue entries. Read from `structural.toml`
// (`Config::load`), or built by an embedder and returned from a
// `config::Source` (docs/servers.md).
#[derive(Debug, Default, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct Config {
    /// Servers to adjust, add, or leave out, by name. Naming `rust-analyzer`
    /// or `pyright` changes only the keys given, and the rest of their
    /// built-in handling stays. Any other name describes a server in full,
    /// so `command` and `languages` are required, and replaces any known
    /// server of that name. A server starts only in a project with files in
    /// one of its languages.
    #[serde(default)]
    pub servers: BTreeMap<String, ServerEntry>,
    /// Seconds a running server may go unused — no query routed to it, no
    /// agent starting work in this project — before it is stopped. The
    /// next query that needs it starts it again. Default `1800` (30
    /// minutes).
    // A resource knob, not selection policy: it rides on `Config` because
    // this is where every workspace-level key lives (docs/observation.md).
    #[serde(default)]
    pub idle_eviction_secs: Option<u64>,
}

impl Config {
    /// The idle-eviction interval this configuration asks for, or the
    /// default.
    pub fn idle_eviction(&self) -> Duration {
        self.idle_eviction_secs
            .map(Duration::from_secs)
            .unwrap_or(DEFAULT_IDLE_EVICTION)
    }
}

/// One language server: a built-in to adjust, a server to add, or one to
/// leave out of this project with `disabled = true`.
#[derive(Debug, Default, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, rename_all = "kebab-case")]
pub struct ServerEntry {
    /// The command that starts the server, executable first, as in
    /// `["gopls", "serve"]`. Required for a server other than
    /// `rust-analyzer` and `pyright`; for those two it replaces the built-in
    /// command.
    #[serde(default)]
    pub command: Vec<String>,
    /// The languages the server serves, by language id, as in `["go"]`.
    /// Required for a server other than `rust-analyzer` and `pyright`; for
    /// those two it replaces their built-in languages. Common ids such as
    /// `go`, `typescript` and `cpp` come with their file extensions; any
    /// other id needs `extensions`.
    #[serde(default)]
    pub languages: Vec<String>,
    /// File extensions, without the dot, for a language id in `languages`,
    /// as in `{ go = ["go"] }`. Needed only for a language without known
    /// extensions; given, they replace the known ones.
    #[serde(default)]
    pub extensions: BTreeMap<String, Vec<String>>,
    /// Leave this server out of this project, whether it is a known server
    /// or one configured here. A disabled entry needs no other key. Default
    /// `false`.
    // Never validated: `disabled = true` on a server with no `command` is a
    // statement about this project, not an incomplete definition
    // (docs/discovery.md).
    #[serde(default)]
    pub disabled: bool,
    /// Seconds the server may stay silent, unanswered probes included, while
    /// a query waits on it, before it is reported broken. Progress
    /// the server reports counts as activity, so a server that is slow but
    /// working is not cut off. Default `90`.
    // The inactivity backstop, the only timer on a query
    // (docs/correctness.md). The hover text states `DEFAULT_BACKSTOP`, and a
    // test holds the two together.
    pub backstop_secs: Option<u64>,
    /// Whether a progress token the server reserves counts as work under
    /// way, which queries wait for. Default `true`. Set `false` for a server
    /// that reserves a token and never reports progress through it, as ccls
    /// does; otherwise every query on it waits indefinitely.
    // Whether `window/workDoneProgress/create` alone counts as outstanding
    // work. ccls creates one `index` token at startup and sends no
    // `$/progress` at all, so counting that create leaves the connection
    // never idle (docs/servers.md).
    pub progress_create_is_work: Option<bool>,
    /// Settings answered to each `workspace/configuration` request the
    /// server sends, the same value for every section it asks about.
    /// Default: none, which leaves the server its own defaults. Some servers
    /// serve nothing without it: deno needs `{ enable = true }`.
    /// rust-analyzer never asks, so this has no effect on it.
    // One copy per requested item (docs/servers.md). JSON because that is
    // what goes on the wire; a TOML table deserializes into it. The
    // `additionalProperties` is for Tombi, whose strict mode reads a table
    // with no declared keys as closed and warns on every key in it.
    #[schemars(extend("additionalProperties" = true))]
    pub settings: Option<Value>,
}

#[cfg(test)]
mod tests;
