//! rust-analyzer: everything the generic transport must not know about it.

use super::{Adapter, Language, ReadinessPolicy, ViewSite, default_kind_label};
use lsp_types::{Position, Range, SymbolKind};
use serde_json::{Value, json};
use std::path::Path;

mod outline;
mod views;

pub struct RustAnalyzer;

/// `workspace.symbol.search.limit`. rust-analyzer orders a symbol search's
/// matches by name and truncates at this many, default 128: with `fn
/// tool_call` beside two hundred `fn a_tool_call_NNN`, the exact match sat
/// at position 203 of 401 and was cut, and the query answered "no exact
/// match" about a function plainly in the tree. Bounded rather than
/// unbounded because a one-letter query matches nearly every symbol: on a
/// 4,500-file tree, unbounded `e` answered 122,599 matches in 31.9 MB —
/// half the transport's frame cap — where 1024 answers about 270 KB in
/// 0.3 to 2.7 s once the index is built, measured on a heavily loaded
/// machine. An answer at the limit is
/// marked as truncated rather than read as complete (docs/resolution.md;
/// journal/2026-10-20-readiness-generation.md).
pub const SYMBOL_SEARCH_LIMIT: usize = 1024;

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
    ///
    /// The limit replaces the default of 128 ([`SYMBOL_SEARCH_LIMIT`]).
    fn initialization_options(&self) -> Value {
        json!({
            "workspace": {
                "symbol": {
                    "search": {
                        "kind": "all_symbols",
                        "excludeImports": true,
                        "limit": SYMBOL_SEARCH_LIMIT,
                    }
                }
            }
        })
    }

    /// The limit set at `initialize`: an answer of that many is truncated.
    fn symbol_result_cap(&self) -> Option<usize> {
        Some(SYMBOL_SEARCH_LIMIT)
    }

    /// rust-analyzer answers a request with its result type's default —
    /// `null` for these — while its file roots are loading, and `[]` once
    /// settled with nothing to report: measured on 1.101.0-nightly for
    /// `workspace/symbol` with no match, and `definition` and
    /// `implementation` at a keyword and at whitespace. Not
    /// `references`, whose settled answer away from a name is `null` too
    /// (docs/servers.md).
    fn null_is_unloaded(&self, method: &str) -> bool {
        matches!(
            method,
            "workspace/symbol" | "textDocument/definition" | "textDocument/implementation"
        )
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

    fn render_extra_view(
        &self,
        view: &str,
        result: &Value,
        root: &Path,
        site: Option<ViewSite<'_>>,
    ) -> anyhow::Result<String> {
        views::render(view, result, root, site)
    }

    /// Offered in this order; rust-analyzer takes the first it supports.
    fn position_encodings(&self) -> Vec<&'static str> {
        vec!["utf-8", "utf-16"]
    }

    /// rust-analyzer nests a method under an `impl` node, not under the type
    /// it is written for: the node is named `impl Config`, `impl Label for
    /// Config` or `impl<T> Wrapper<T>`, and its selection range names the
    /// type — the container its `workspace/symbol` reports for the methods
    /// under it. The sibling `outline` module parses that header and
    /// compares the self type it names with the range exactly; an ordinary
    /// name is its own container.
    fn outline_container(&self, name: &str, selected: &str) -> Option<String> {
        outline::container(name, selected)
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

    /// rust-analyzer resolves intra-doc links (`[`fn_name`]`) but its
    /// reference search does not visit doc comments, so its rename edit
    /// covers code only (docs/servers.md): the answer carries this caveat
    /// so the caller greps the docs rather than trusting full coverage
    /// (docs/tools.md).
    fn rename_note(&self, old_name: &str) -> Option<String> {
        Some(format!(
            "rust-analyzer rename may leave intra-doc links unchanged; check doc comments for `{old_name}`."
        ))
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

    /// The seam the resolution layer calls, through the real adapter: an
    /// ordinary name is its own container, an impl node's is the self type
    /// its header names, and nothing else is a container at all. The parse
    /// itself is covered in [`outline`]'s tests.
    #[test]
    fn the_container_rule_is_exact_and_never_a_substring() {
        let adapter = RustAnalyzer;
        assert_eq!(
            adapter.outline_container("Config", "Config"),
            Some("Config".to_string())
        );
        assert_eq!(
            adapter.outline_container("implementation", "implementation"),
            Some("implementation".to_string()),
            "an ordinary name beginning `impl` still matches itself"
        );
        assert_eq!(
            adapter.outline_container("impl Label for Config", "Config"),
            Some("Config".to_string())
        );
        for (name, selected) in [
            ("Config", "Other"),
            ("OtherItem", "Item"),
            ("implementation", "mentation"),
            ("impl Label for Config", "Label"),
            ("impl<T> Wrapper<T>", "T"),
            ("impl crate::config::Config", "Config"),
        ] {
            assert_eq!(
                adapter.outline_container(name, selected),
                None,
                "{name} selecting {selected} must not be a container"
            );
        }
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
        // The cap it declares is the limit it asks for.
        assert_eq!(
            options.pointer("/workspace/symbol/search/limit"),
            Some(&json!(SYMBOL_SEARCH_LIMIT))
        );
        assert_eq!(adapter.symbol_result_cap(), Some(SYMBOL_SEARCH_LIMIT));
    }

    /// Only the methods measured to answer `[]` when settled read `null` as
    /// an unloaded index; `references` answers `null` settled too.
    #[test]
    fn null_means_unloaded_only_where_measured() {
        let adapter = RustAnalyzer;
        for method in [
            "workspace/symbol",
            "textDocument/definition",
            "textDocument/implementation",
        ] {
            assert!(adapter.null_is_unloaded(method), "{method}");
        }
        for method in [
            "textDocument/references",
            "textDocument/hover",
            "textDocument/rename",
        ] {
            assert!(!adapter.null_is_unloaded(method), "{method}");
        }
    }
    #[test]
    fn the_rename_answer_carries_the_doc_link_caveat() {
        // rust-analyzer's rename edit does not cover intra-doc links
        // (docs/servers.md); the note interpolates the old name and says
        // "may", so it stays true if a version ever rewrites one.
        let adapter = RustAnalyzer;
        assert_eq!(
            adapter.rename_note("project_with").as_deref(),
            Some(
                "rust-analyzer rename may leave intra-doc links unchanged; check doc comments for `project_with`."
            )
        );
    }
}
