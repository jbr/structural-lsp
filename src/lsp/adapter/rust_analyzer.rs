//! rust-analyzer: everything the generic transport must not know about it.

use super::{Adapter, Language, ReadinessPolicy, default_kind_label};
use lsp_types::{Position, Range, SymbolKind};
use serde_json::{Value, json};
use std::path::Path;

mod views;

pub struct RustAnalyzer;

impl Adapter for RustAnalyzer {
    fn name(&self) -> &str {
        "rust-analyzer"
    }

    fn command(&self) -> Vec<String> {
        vec!["rust-analyzer".to_string()]
    }

    fn install_hint(&self) -> String {
        "`rustup component add rust-analyzer`, and ensure it is on PATH".to_string()
    }

    fn languages(&self) -> Vec<Language> {
        vec![Language::new("rust", &["rs"])]
    }

    /// Two findings from the first review (`docs/servers.md`): the default
    /// symbol search looks at types first and only falls back to everything
    /// when that is empty, so a function whose name fuzzy-matches some type
    /// is unreachable; and imports and re-exports are reported as symbols,
    /// which makes every re-exported name ambiguous.
    fn initialization_options(&self) -> Value {
        json!({
            "workspace": {
                "symbol": {
                    "search": { "kind": "all_symbols", "excludeImports": true }
                }
            }
        })
    }

    /// rust-analyzer sends `experimental/serverStatus` only when the client
    /// advertises this; it is what [`Quiescence`] observes. `hoverRange`
    /// lets `textDocument/hover` take a range, for the type of an
    /// expression.
    fn experimental_capabilities(&self) -> Value {
        json!({ "serverStatusNotification": true, "hoverRange": true })
    }

    /// A range wider than a point is sent as the `position`; rust-analyzer
    /// answers with the type of the expression it spans.
    fn hover_params(&self, uri: &str, range: Range) -> Value {
        if range.start == range.end {
            json!({ "textDocument": { "uri": uri }, "position": range.start })
        } else {
            json!({ "textDocument": { "uri": uri }, "position": range })
        }
    }

    fn extra_views(&self) -> Vec<&'static str> {
        views::names()
    }

    fn extra_view_request(
        &self,
        view: &str,
        uri: &str,
        position: Position,
    ) -> Option<(String, Value)> {
        views::request(view, uri, position)
    }

    fn render_extra_view(&self, view: &str, result: &Value, root: &Path) -> anyhow::Result<String> {
        views::render(view, result, root)
    }

    /// Offered in this order; rust-analyzer takes the first it supports.
    fn position_encodings(&self) -> Vec<&'static str> {
        vec!["utf-8", "utf-16"]
    }

    fn readiness_policy(&self) -> Box<dyn ReadinessPolicy> {
        Box::new(Quiescence::default())
    }

    /// `-> Self` and `Self { .. }` inside an impl are reported as references
    /// to the type (seen in the spike's raw capture for the fixture's
    /// `Config`); a trait's `Self` likewise.
    fn reference_aliases(&self) -> Vec<&'static str> {
        vec!["Self"]
    }

    fn kind_label(&self, kind: SymbolKind) -> &'static str {
        match kind {
            SymbolKind::FUNCTION | SymbolKind::METHOD => "fn",
            SymbolKind::INTERFACE => "trait",
            SymbolKind::MODULE => "mod",
            SymbolKind::CONSTANT => "const",
            SymbolKind::VARIABLE => "static",
            SymbolKind::ENUM_MEMBER => "variant",
            SymbolKind::TYPE_PARAMETER => "type",
            SymbolKind::OBJECT => "impl",
            other => default_kind_label(other),
        }
    }
}

/// Ready only once rust-analyzer's latest `experimental/serverStatus` says
/// `quiescent: true`. Composed with standard progress by the transport, so a
/// quiescent status while a progress token is still open does not count.
///
/// Before the first status arrives the server is not ready: a cold start
/// blocks until rust-analyzer has finished its initial load, which is the
/// point. A later `quiescent: false` (re-indexing after an edit, a
/// `cargo check`) makes it not ready again until the next `true`.
#[derive(Debug, Default)]
pub struct Quiescence {
    quiescent: Option<bool>,
}

impl ReadinessPolicy for Quiescence {
    fn observe(&mut self, method: &str, params: Option<&Value>) {
        if method != "experimental/serverStatus" {
            return;
        }
        if let Some(quiescent) = params
            .and_then(|p| p.get("quiescent"))
            .and_then(Value::as_bool)
        {
            self.quiescent = Some(quiescent);
        }
    }

    fn is_ready(&self) -> bool {
        self.quiescent == Some(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(quiescent: bool) -> Value {
        json!({ "health": "ok", "quiescent": quiescent })
    }

    #[test]
    fn not_ready_until_a_quiescent_status_arrives() {
        let mut policy = Quiescence::default();
        assert!(
            !policy.is_ready(),
            "no status yet: a cold start is not ready"
        );
        policy.observe("$/progress", Some(&json!({ "token": 1 })));
        assert!(!policy.is_ready(), "other notifications say nothing");
        policy.observe("experimental/serverStatus", Some(&status(true)));
        assert!(policy.is_ready());
    }

    #[test]
    fn follows_the_latest_status() {
        let mut policy = Quiescence::default();
        policy.observe("experimental/serverStatus", Some(&status(true)));
        policy.observe("experimental/serverStatus", Some(&status(false)));
        assert!(
            !policy.is_ready(),
            "a later false supersedes the earlier true"
        );
        policy.observe("experimental/serverStatus", Some(&status(true)));
        assert!(policy.is_ready());
        // A status without the flag changes nothing.
        policy.observe(
            "experimental/serverStatus",
            Some(&json!({ "health": "warning" })),
        );
        assert!(policy.is_ready());
    }

    #[test]
    fn the_adapter_asks_for_the_status_and_the_full_symbol_index() {
        let adapter = RustAnalyzer;
        assert_eq!(
            adapter
                .client_capabilities()
                .pointer("/experimental/serverStatusNotification"),
            Some(&json!(true))
        );
        let options = adapter.initialization_options();
        assert_eq!(
            options.pointer("/workspace/symbol/search/kind"),
            Some(&json!("all_symbols"))
        );
        assert_eq!(
            options.pointer("/workspace/symbol/search/excludeImports"),
            Some(&json!(true))
        );
    }
}
