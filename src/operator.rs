//! The operator channel: facts about the host that the calling agent cannot
//! act on, pushed to whoever runs structural (`docs/operator.md`).
//!
//! A tool answer carries what its caller can act on — a scope with no
//! symbol index, a server that failed for the file asked about. What is left
//! is the operator's: a server that would not start and why, a ladder that
//! fell back, a language in the tree that nothing serves, a server the idle
//! sweep released, a watcher that was lost. Those go to one [`Operator`] the
//! host is constructed with ([`crate::host::Host::with_operator`]), as
//! structured [`Event`]s, and only at transitions: nothing here is emitted
//! per query.
//!
//! The default sink is [`Stderr`]: one line per event, prefixed
//! `structural:`. The MCP front end's sink is [`mcp::Mcp`], which writes the
//! same line and also sends each event to the client as MCP logging.

use std::fmt;
use std::path::{Path, PathBuf};

pub mod mcp;

/// How much an event asks of an operator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Level {
    /// The host changed state in a way an operator may want to see — a
    /// server released, a watch recovered — and nothing is wrong.
    Info,
    /// Something is degraded and the host is working around it: a fallback
    /// serving, a candidate skipped, a language with no server, a watch lost.
    Warning,
    /// Something an operator has to fix before it will work: a ladder with
    /// no candidate that starts, a configuration that does not load.
    Error,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Info => "info",
            Level::Warning => "warning",
            Level::Error => "error",
        }
    }
}

impl fmt::Display for Level {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Which transition an event reports. Carried beside the message so an
/// embedder can route or render by kind without parsing prose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Kind {
    /// A candidate failed to start and the ladder goes on to the next one.
    CandidateFailed,
    /// A launch ended with no server: every candidate on the ladder failed,
    /// or the started server could not be published.
    LaunchFailed,
    /// A server is serving in place of the candidates above it that failed.
    Fallback,
    /// The tree gives evidence for a language no plan serves.
    Unserved,
    /// The workspace's configuration — a present `structural.toml`, or
    /// the embedder's source — does not load, so nothing starts.
    Config,
    /// The idle sweep stopped a server no strong signal had addressed.
    Released,
    /// The workspace's filesystem watch failed or was lost.
    WatchLost,
    /// The workspace's filesystem watch is back.
    WatchRecovered,
}

impl Kind {
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::CandidateFailed => "candidate-failed",
            Kind::LaunchFailed => "launch-failed",
            Kind::Fallback => "fallback",
            Kind::Unserved => "unserved",
            Kind::Config => "config",
            Kind::Released => "released",
            Kind::WatchLost => "watch-lost",
            Kind::WatchRecovered => "watch-recovered",
        }
    }
}

/// One fact for the operator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    pub level: Level,
    pub kind: Kind,
    /// The workspace it happened in.
    pub root: PathBuf,
    /// The language server it is about, when it is about one.
    pub server: Option<String>,
    /// What happened, in one line.
    pub message: String,
}

impl Event {
    pub fn new(level: Level, kind: Kind, root: &Path, message: impl Into<String>) -> Self {
        Self {
            level,
            kind,
            root: root.to_path_buf(),
            server: None,
            message: message.into(),
        }
    }

    pub fn with_server(mut self, server: impl Into<String>) -> Self {
        self.server = Some(server.into());
        self
    }
}

/// `structural: <level>: <root>: [<server>: ]<message>`, on one line: a
/// line break inside a reason (a server's multi-line error) becomes a space.
impl fmt::Display for Event {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "structural: {}: {}: ", self.level, self.root.display())?;
        if let Some(server) = &self.server {
            write!(f, "{server}: ")?;
        }
        f.write_str(&one_line(&self.message))
    }
}

/// Collapse every line break, and the indentation after it, to one space.
fn one_line(text: &str) -> String {
    text.split('\n')
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Where operator events go. Set once, when the host is constructed.
///
/// Called on whichever thread made the transition — a launcher, the
/// observation thread, a binding — sometimes while that thread holds a
/// workspace's slot list or its lock, so an implementation returns promptly
/// and never calls back into the host.
pub trait Operator: Send + Sync + 'static {
    fn event(&self, event: &Event);
}

/// The default sink: each event as one line on the process's stderr.
#[derive(Debug, Default, Clone, Copy)]
pub struct Stderr;

impl Operator for Stderr {
    fn event(&self, event: &Event) {
        eprintln!("{event}");
    }
}

#[cfg(test)]
mod tests;
