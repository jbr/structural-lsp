//! Capturing raw query results and writing the report files.

use super::*;

/// The outcome of one request, captured uniformly for the raw log.
pub(super) struct Captured {
    pub(super) result: Value,
    pub(super) error: Option<String>,
    pub(super) elapsed_ms: u128,
}

/// Counts of code-action shapes seen, so a menu that is all commands is
/// distinguishable from one whose edits are deferred behind `codeAction/resolve`.
#[derive(Default)]
pub(super) struct ActionTally {
    pub(super) edit_bearing: usize,
    pub(super) command_only: usize,
    pub(super) unresolved: usize,
    pub(super) resolved: usize,
    pub(super) other: usize,
}

/// Run one request, capturing result, error, and latency uniformly.
pub(super) fn capture(handle: &ServerHandle, method: &str, params: Value) -> Captured {
    let began = Instant::now();
    let (result, error) = match handle.request(method, params) {
        Ok(value) => (value, None),
        Err(error) => (Value::Null, Some(format!("{error:#}"))),
    };
    Captured {
        result,
        error,
        elapsed_ms: began.elapsed().as_millis(),
    }
}

/// The most recent `experimental/serverStatus` params in the event log, or
/// `Null` when the server advertises none (pyright).
pub(super) fn latest_server_status(handle: &ServerHandle) -> Result<Value> {
    Ok(handle
        .capture_events()?
        .iter()
        .rev()
        .find(|event| {
            event.get("method").and_then(Value::as_str) == Some("experimental/serverStatus")
        })
        .and_then(|event| event.get("params").cloned())
        .unwrap_or(Value::Null))
}

/// Write one captured query to `raw/`, unmodified.
pub(super) fn write_raw(
    raw: &Path,
    index: usize,
    meta: &AnnotationMeta,
    method: &str,
    params: &Value,
    captured: &Captured,
) -> Result<()> {
    let file = raw.join(format!(
        "{index:03}-{}-{}-{}.json",
        meta.kind,
        slug(&meta.query),
        slug(method)
    ));
    fs::write(
        &file,
        serde_json::to_vec_pretty(&json!({
            "annotation": meta.json(),
            "method": method,
            "params": params,
            "elapsed_ms": captured.elapsed_ms,
            "ok": captured.error.is_none(),
            "error": captured.error,
            "result": captured.result,
        }))?,
    )?;
    Ok(())
}

/// A code action whose edit is deferred behind `codeAction/resolve`.
pub(super) fn is_unresolved(action: &Value) -> bool {
    action.get("data").is_some() && action.get("edit").is_none()
}

impl ActionTally {
    pub(super) fn classify(&mut self, action: &Value) {
        if action.get("edit").is_some() {
            self.edit_bearing += 1;
        } else if action.get("command").is_some() {
            self.command_only += 1;
        } else if action.get("data").is_some() {
            self.unresolved += 1;
        } else {
            self.other += 1;
        }
    }
}

pub(super) fn is_nonempty(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Array(items) => !items.is_empty(),
        Value::Object(map) => !map.is_empty(),
        Value::Bool(_) | Value::Number(_) | Value::String(_) => true,
    }
}

pub(super) fn slug(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        out.push_str("empty");
    }
    out
}

pub(super) fn epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}
