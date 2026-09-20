//! Standard `$/progress` token tracking: the transport's own readiness signal.

use serde_json::Value;
use std::collections::HashSet;

/// The set of progress tokens the server has opened and not ended.
/// `window/workDoneProgress/create` and a `$/progress` `begin` both open a
/// token; `end` closes it; `report` only proves the server is alive.
///
/// Counting `create` is conservative beyond the spec, which makes it a
/// *reservation*: the server may report under this token, and work begins at
/// `begin`. It is counted because a server that creates a token is about to
/// start work, and the gap before `begin` is a window in which this
/// connection would otherwise call itself idle. `create_is_work` is how a
/// server that reserves a token it never reports through says so
/// (`../../docs/servers.md`); without it, such a connection is never idle
/// and every readiness wait on it blocks forever.
pub(super) struct ProgressTokens {
    pub(super) open: HashSet<String>,
    pub(super) cap: usize,
    /// Whether `window/workDoneProgress/create` alone opens a token.
    create_is_work: bool,
}

impl ProgressTokens {
    pub(super) fn new(cap: usize, create_is_work: bool) -> Self {
        Self {
            open: HashSet::new(),
            cap,
            create_is_work,
        }
    }

    pub(super) fn is_idle(&self) -> bool {
        self.open.is_empty()
    }

    pub(super) fn len(&self) -> usize {
        self.open.len()
    }

    /// Fold one server-initiated request or notification into the set.
    /// `Err` means the server exceeded the token cap.
    pub(super) fn observe(&mut self, method: &str, params: Option<&Value>) -> Result<(), String> {
        let token = params.and_then(|p| p.get("token")).map(canonical_token);
        match method {
            "window/workDoneProgress/create" => match token {
                Some(token) if self.create_is_work => self.open_token(token),
                _ => Ok(()),
            },
            "$/progress" => {
                let kind = params
                    .and_then(|p| p.pointer("/value/kind"))
                    .and_then(Value::as_str);
                match (kind, token) {
                    (Some("begin"), Some(token)) => self.open_token(token),
                    (Some("end"), Some(token)) => {
                        self.open.remove(&token);
                        Ok(())
                    }
                    _ => Ok(()),
                }
            }
            _ => Ok(()),
        }
    }

    pub(super) fn open_token(&mut self, token: String) -> Result<(), String> {
        if !self.open.contains(&token) && self.open.len() >= self.cap {
            return Err(format!(
                "server opened more than {} progress tokens at once",
                self.cap
            ));
        }
        self.open.insert(token);
        Ok(())
    }
}

/// Tokens are strings or integers on the wire; key both the same way.
pub(super) fn canonical_token(token: &Value) -> String {
    match token {
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::ProgressTokens;
    use serde_json::json;

    #[test]
    fn create_begin_and_end_track_the_open_set() {
        let mut tokens = ProgressTokens::new(8, true);
        assert!(tokens.is_idle());
        let create = json!({ "token": "a" });
        tokens
            .observe("window/workDoneProgress/create", Some(&create))
            .unwrap();
        assert_eq!(tokens.len(), 1, "create opens the token");
        let begin = json!({ "token": "a", "value": { "kind": "begin", "title": "x" } });
        tokens.observe("$/progress", Some(&begin)).unwrap();
        assert_eq!(
            tokens.len(),
            1,
            "begin on a created token is not a second token"
        );
        let report = json!({ "token": "a", "value": { "kind": "report" } });
        tokens.observe("$/progress", Some(&report)).unwrap();
        assert!(!tokens.is_idle(), "report keeps it open");
        let end = json!({ "token": "a", "value": { "kind": "end" } });
        tokens.observe("$/progress", Some(&end)).unwrap();
        assert!(tokens.is_idle(), "end closes it");
        // Integer and string tokens key the same way.
        let begin = json!({ "token": 7, "value": { "kind": "begin" } });
        tokens.observe("$/progress", Some(&begin)).unwrap();
        let end = json!({ "token": "7", "value": { "kind": "end" } });
        tokens.observe("$/progress", Some(&end)).unwrap();
        assert!(tokens.is_idle());
    }

    /// ccls sends one `window/workDoneProgress/create` at startup, for a
    /// token it never begins and never ends, whatever the size of the tree
    /// (`../../journal/2026-09-19-ccls-reserves-a-token.md`). Counting that
    /// create leaves the connection permanently not-idle.
    #[test]
    fn a_reservation_is_not_work_when_the_server_never_reports_through_it() {
        let mut tokens = ProgressTokens::new(8, false);
        let create = json!({ "token": "index" });
        tokens
            .observe("window/workDoneProgress/create", Some(&create))
            .unwrap();
        assert!(tokens.is_idle(), "the reservation alone is not work");
        // Progress the server does report is still tracked, both ends.
        let begin = json!({ "token": "index", "value": { "kind": "begin" } });
        tokens.observe("$/progress", Some(&begin)).unwrap();
        assert!(!tokens.is_idle(), "begin is work whatever opened it");
        let end = json!({ "token": "index", "value": { "kind": "end" } });
        tokens.observe("$/progress", Some(&end)).unwrap();
        assert!(tokens.is_idle());
    }

    #[test]
    fn exceeding_the_token_cap_is_an_error() {
        let mut tokens = ProgressTokens::new(2, true);
        for token in ["a", "b"] {
            let begin = json!({ "token": token, "value": { "kind": "begin" } });
            tokens.observe("$/progress", Some(&begin)).unwrap();
        }
        let begin = json!({ "token": "c", "value": { "kind": "begin" } });
        let error = tokens.observe("$/progress", Some(&begin)).unwrap_err();
        assert!(error.contains("more than 2"), "{error}");
        // Re-opening an already open token is not a new token.
        let begin = json!({ "token": "a", "value": { "kind": "begin" } });
        tokens.observe("$/progress", Some(&begin)).unwrap();
    }
}
