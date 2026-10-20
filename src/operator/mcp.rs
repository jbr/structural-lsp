//! The MCP front end's operator sink: every event on stderr, as [`super::Stderr`]
//! writes it, and as an MCP log message (`notifications/message`) to the
//! client once the connection can carry one (`docs/operator.md`).
//!
//! The binding's eager warm runs before the MCP connection exists, so the
//! first launch failures happen with nowhere to send them. Until
//! [`Mcp::attach`] names the connection's notifier, events are held — the
//! first [`HELD`] of them; past that only a count is kept — and attaching
//! sends them, in order, before anything later. Stderr gets every event at
//! once whether or not it is held.

use super::{Event, Level, Operator};
use mcplease::types::{LoggingLevel, LoggingMessage, Notifier};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

/// How many events are held for a connection that is not yet attached.
/// Past this, an event still reaches stderr and is counted, and attaching
/// sends one warning saying how many were not held.
pub const HELD: usize = 64;

/// The `logger` every message names.
pub const LOGGER: &str = "structural";

/// Stderr always, and MCP logging once attached.
#[derive(Default)]
pub struct Mcp {
    state: Mutex<State>,
}

#[derive(Default)]
struct State {
    notifier: Option<Arc<dyn Notifier>>,
    held: Vec<Event>,
    /// Events past [`HELD`] before attaching: on stderr only.
    unheld: usize,
}

impl Mcp {
    pub fn new() -> Self {
        Self::default()
    }

    /// Send events to `notifier` from now on, after the ones held so far.
    /// The first notifier attached is kept; a later call does nothing.
    ///
    /// The notifier decides what reaches the client: mcplease's connection
    /// notifier drops everything before the `initialize` handshake completes
    /// and every message below the level the client set with
    /// `logging/setLevel`.
    pub fn attach(&self, notifier: Arc<dyn Notifier>) {
        let mut state = self.lock();
        if state.notifier.is_some() {
            return;
        }
        for event in std::mem::take(&mut state.held) {
            notifier.notify(message(&event).into_notification());
        }
        if state.unheld > 0 {
            let count = std::mem::take(&mut state.unheld);
            notifier.notify(
                LoggingMessage::new(
                    LoggingLevel::Warning,
                    json!({
                        "kind": "unheld",
                        "message": format!(
                            "{count} earlier event{} arrived past the {HELD} held for this \
                             connection and went to stderr only",
                            if count == 1 { "" } else { "s" }
                        ),
                    }),
                )
                .with_logger(LOGGER)
                .into_notification(),
            );
        }
        state.notifier = Some(notifier);
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl Operator for Mcp {
    fn event(&self, event: &Event) {
        eprintln!("{event}");
        // Under the lock, so an event cannot pass the held ones that
        // `attach` is sending.
        let mut state = self.lock();
        match &state.notifier {
            Some(notifier) => notifier.notify(message(event).into_notification()),
            None if state.held.len() < HELD => state.held.push(event.clone()),
            None => state.unheld += 1,
        }
    }
}

/// The MCP log message for one event: its level, the `structural` logger,
/// and the event's fields as `data`.
pub fn message(event: &Event) -> LoggingMessage {
    let level = match event.level {
        Level::Info => LoggingLevel::Info,
        Level::Warning => LoggingLevel::Warning,
        Level::Error => LoggingLevel::Error,
    };
    let mut data = json!({
        "kind": event.kind.as_str(),
        "root": event.root.display().to_string(),
        "message": event.message,
    });
    if let (Some(server), Value::Object(fields)) = (&event.server, &mut data) {
        fields.insert("server".to_string(), json!(server));
    }
    LoggingMessage::new(level, data).with_logger(LOGGER)
}
