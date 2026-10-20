//! The event's line, and the MCP sink's hold-then-flush.

use super::mcp::{HELD, Mcp};
use super::*;
use mcplease::types::{JsonRpcNotification, Notifier};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

/// A notifier that keeps what it is given.
#[derive(Default)]
struct Kept(Mutex<Vec<JsonRpcNotification>>);

impl Notifier for Kept {
    fn notify(&self, notification: JsonRpcNotification) {
        self.0.lock().unwrap().push(notification);
    }
}

impl Kept {
    fn params(&self) -> Vec<Value> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .map(|notification| {
                assert_eq!(notification.method, "notifications/message");
                notification.params.clone().unwrap_or_default()
            })
            .collect()
    }
}

fn event(n: usize) -> Event {
    Event::new(
        Level::Warning,
        Kind::CandidateFailed,
        Path::new("/w"),
        format!("event {n}"),
    )
}

#[test]
fn an_event_is_one_line_naming_level_root_and_server() {
    let event = Event::new(
        Level::Error,
        Kind::LaunchFailed,
        Path::new("/work/project"),
        "2 candidates were tried and none started:\n    a: gone\n    b: gone",
    )
    .with_server("a");
    assert_eq!(
        event.to_string(),
        "structural: error: /work/project: a: 2 candidates were tried and none started: \
         a: gone b: gone"
    );
    let unattributed = Event::new(Level::Info, Kind::WatchRecovered, Path::new("/w"), "back");
    assert_eq!(unattributed.to_string(), "structural: info: /w: back");
}

#[test]
fn the_mcp_message_carries_the_event_as_data() {
    let message = mcp::message(&event(1).with_server("pyright"));
    assert_eq!(
        serde_json::to_value(&message).unwrap(),
        json!({
            "level": "warning",
            "logger": "structural",
            "data": {
                "kind": "candidate-failed",
                "root": "/w",
                "server": "pyright",
                "message": "event 1",
            },
        })
    );
}

#[test]
fn events_before_attach_are_held_and_sent_first_in_order() {
    let sink = Mcp::new();
    sink.event(&event(1));
    sink.event(&event(2));
    let kept = Arc::new(Kept::default());
    sink.attach(Arc::clone(&kept) as Arc<dyn Notifier>);
    sink.event(&event(3));
    let messages: Vec<Value> = kept
        .params()
        .into_iter()
        .map(|params| params["data"]["message"].clone())
        .collect();
    assert_eq!(messages, ["event 1", "event 2", "event 3"]);

    // The first notifier is kept: a second attach neither replays nor
    // redirects.
    let second = Arc::new(Kept::default());
    sink.attach(Arc::clone(&second) as Arc<dyn Notifier>);
    sink.event(&event(4));
    assert!(second.params().is_empty());
    assert_eq!(kept.params().len(), 4);
}

#[test]
fn past_the_bound_only_a_count_is_kept_and_attach_says_so() {
    let sink = Mcp::new();
    for n in 0..HELD + 3 {
        sink.event(&event(n));
    }
    let kept = Arc::new(Kept::default());
    sink.attach(Arc::clone(&kept) as Arc<dyn Notifier>);
    let params = kept.params();
    assert_eq!(params.len(), HELD + 1, "the held events and one count");
    assert_eq!(params[0]["data"]["message"], "event 0");
    assert_eq!(
        params[HELD - 1]["data"]["message"],
        format!("event {}", HELD - 1)
    );
    let count = &params[HELD];
    assert_eq!(count["level"], "warning");
    assert_eq!(count["data"]["kind"], "unheld");
    assert!(
        count["data"]["message"]
            .as_str()
            .unwrap()
            .starts_with("3 earlier events"),
        "{count}"
    );
}
