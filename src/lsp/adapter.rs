//! Per-server knowledge: the seam between the generic transport and what
//! each language server needs said to it.
//!
//! The transport ([`crate::lsp::handle`]) tracks *standard* readiness
//! itself: the set of `window/workDoneProgress/create` / `$/progress` tokens
//! the server has opened and not ended ("standard idle" = the set is
//! empty). A [`ReadinessPolicy`] may add a stronger, server-specific
//! condition; the transport composes readiness as
//!
//! ```text
//! ready = standard_idle && policy.is_ready()
//! ```
//!
//! so a policy **cannot** override active standard progress. A policy that
//! wants a server-specific signal observes the notification stream through
//! [`ReadinessPolicy::observe`] and keeps its own protocol state — it never
//! rescans the event log, which is capped and diagnostics-only.
//!
//! An [`Adapter`] is everything else the generic server
//! ([`crate::lsp::server::LspServer`]) asks about one kind of language
//! server: how to launch it, which files it serves, what to say at
//! `initialize`, how to rewrite a symbol query, and which readiness policy
//! to run. Every method has a default; the built-ins are
//! [`rust_analyzer::RustAnalyzer`] and [`pyright::Pyright`], and
//! [`configured::Configured`] is any command named in `structural.toml`.
//! No server-specific string belongs anywhere else.

use serde_json::{Value, json};
use std::sync::Arc;
use std::time::Duration;

pub mod configured;
pub mod pyright;
pub mod rust_analyzer;

/// A server's readiness policy beyond standard progress tracking.
///
/// All methods have defaults; [`StandardIdle`] is the "standard progress
/// only" policy. The transport calls `observe` for every notification the
/// server sends, in arrival order, before deciding readiness; `is_ready` is
/// consulted with the AND of standard idle described on the module.
pub trait ReadinessPolicy: Send {
    /// Fold one server notification into the policy's protocol state.
    /// Called on the owner thread for every notification, in order.
    fn observe(&mut self, _method: &str, _params: Option<&Value>) {}

    /// Whether the policy's own (server-specific) readiness condition holds.
    /// Consulted only after standard progress is idle; returning `false`
    /// keeps the connection not-ready, returning `true` never overrides
    /// active standard progress.
    fn is_ready(&self) -> bool {
        true
    }
}

/// The default policy: standard progress tracking only. Adds no condition of
/// its own, so readiness is exactly "no outstanding work" (an empty progress
/// token set) — the right default for servers that send no readiness signal
/// beyond `$/progress` (e.g. pyright, per the spike).
pub struct StandardIdle;

impl ReadinessPolicy for StandardIdle {}

/// A language a server serves: the LSP `languageId` and the file extensions
/// that map to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Language {
    pub id: String,
    pub extensions: Vec<String>,
}

impl Language {
    pub fn new(id: &str, extensions: &[&str]) -> Self {
        Self {
            id: id.to_string(),
            extensions: extensions.iter().map(|e| e.to_string()).collect(),
        }
    }
}

/// The default inactivity backstop: a server that says nothing for this
/// long while work is pending is broken, not busy.
pub const DEFAULT_BACKSTOP: Duration = Duration::from_secs(90);

/// One kind of language server. Implementations are cheap value objects;
/// the registry keeps one per running server.
pub trait Adapter: Send + Sync {
    /// The registry key and the name shown in results: `rust-analyzer`,
    /// `pyright`, or the `[servers.<name>]` from `structural.toml`.
    fn name(&self) -> &str;

    /// The full argv, executable first.
    fn command(&self) -> Vec<String>;

    /// What to tell the user when the executable is not on `PATH`.
    fn install_hint(&self) -> String {
        format!("install `{}` and put it on PATH", self.command()[0])
    }

    /// The languages this server answers for.
    fn languages(&self) -> Vec<Language>;

    /// `initializationOptions` for `initialize`.
    fn initialization_options(&self) -> Value {
        json!({})
    }

    /// The `experimental` block of the client capabilities, or `Null`.
    /// What to answer this server's `workspace/configuration` request
    /// with. `Null` is "no opinion".
    ///
    /// Separate from [`Adapter::initialization_options`] because servers
    /// read the two differently and one does not substitute for the other:
    /// deno 2.9.6 ignores `enable` in the initialization options and serves
    /// nothing for the whole session, and answers every query once the same
    /// `{"enable": true}` arrives as a configuration reply (docs/servers.md).
    fn workspace_configuration(&self) -> Value {
        Value::Null
    }

    fn experimental_capabilities(&self) -> Value {
        Value::Null
    }

    /// Position encodings to offer, in order of preference. The server picks
    /// one; the generic server converts with whichever it picked.
    fn position_encodings(&self) -> Vec<&'static str> {
        vec!["utf-16"]
    }

    /// The client capabilities to send. The default profile is
    /// [`standard_capabilities`] with this adapter's encodings and
    /// experimental block; override only for a server that needs a different
    /// shape.
    fn client_capabilities(&self) -> Value {
        standard_capabilities(&self.position_encodings(), self.experimental_capabilities())
    }

    /// The readiness condition beyond standard progress. Fresh per server.
    fn readiness_policy(&self) -> Box<dyn ReadinessPolicy> {
        Box::new(StandardIdle)
    }

    /// Rewrite a `workspace/symbol` query for this server, when it needs it.
    fn symbol_query(&self, query: &str) -> String {
        query.to_string()
    }

    /// The inactivity backstop for this server's waits and queries.
    fn backstop(&self) -> Duration {
        DEFAULT_BACKSTOP
    }

    /// Whether `window/workDoneProgress/create` alone counts as outstanding
    /// work for this server.
    ///
    /// True for every server that begins the work it reserved a token for:
    /// the create is the first sign of it, and the gap before `begin` is a
    /// window this connection would otherwise call idle. False states an
    /// observation about *this* server — it reserves a token and never
    /// reports through it, so counting the create would leave the connection
    /// never idle and every readiness wait on it blocking forever. ccls is
    /// the measured case (docs/servers.md).
    fn progress_create_is_work(&self) -> bool {
        true
    }

    /// Views beyond the standard ones this server can answer through its
    /// own extensions, by their `view` names.
    fn extra_views(&self) -> Vec<&'static str> {
        Vec::new()
    }

    /// The request (method and params) behind one of [`Adapter::extra_views`]
    /// at a position; `None` for a view this server does not offer.
    fn extra_view_request(
        &self,
        _view: &str,
        _uri: &str,
        _position: lsp_types::Position,
    ) -> Option<(String, Value)> {
        None
    }

    /// Render the result of an extra view's request. `root` is for
    /// relative paths.
    fn render_extra_view(
        &self,
        view: &str,
        _result: &Value,
        _root: &std::path::Path,
    ) -> anyhow::Result<String> {
        anyhow::bail!("{} has no view named `{view}`", self.name())
    }

    /// `textDocument/hover` params over `range`: the standard request takes
    /// a position, so the start; a server that accepts a range (rust-
    /// analyzer's `hoverRange`) overrides this.
    fn hover_params(&self, uri: &str, range: lsp_types::Range) -> Value {
        json!({ "textDocument": { "uri": uri }, "position": range.start })
    }

    /// Tokens this server reports as references to a symbol without
    /// spelling its name (rust-analyzer: `Self`). A reference whose text is
    /// neither the name nor one of these is a stale range.
    fn reference_aliases(&self) -> Vec<&'static str> {
        Vec::new()
    }

    /// A one-line caveat attached to this server's rename previews and
    /// applied summaries, when its rename has a known blind spot the
    /// caller should check by hand (docs/tools.md). `None` by default: no
    /// claim either way. The caveat is this server's standing policy, not
    /// a statement about any one edit — a rename whose edit does rewrite
    /// doc links must still be able to carry it truthfully, so the wording
    /// says "may", and the old name is interpolated here where the words
    /// live.
    fn rename_note(&self, old_name: &str) -> Option<String> {
        let _ = old_name;
        None
    }

    /// The **container** a symbol's enclosing outline node gives it, from
    /// that node's advertised `name` and the text `selected` at its
    /// selection range, or `None` when this server's outline gives no
    /// container for such a node.
    ///
    /// The default is an ordinary symbol: its selection range names the
    /// symbol itself, so the advertised name is the container and any other
    /// text is a disagreement this cannot resolve. A server whose outline
    /// nests a method under a node that names something besides itself
    /// (rust-analyzer's `impl Config`, whose range names `Config`) overrides
    /// this with the rule its own outline follows.
    ///
    /// A container is only ever the qualifier filter's input, so an
    /// unrecognized shape must yield `None` or a name the server itself
    /// reports — never a substring of the ancestor's name that happens to
    /// look plausible.
    ///
    /// The default accepts a selection range **wider than the name**, on
    /// the same terms `locate_name_at` accepts one from the symbol index
    /// (docs/correctness.md): the advertised name must occur in the
    /// selected text exactly once and at identifier boundaries. R's
    /// `languageserver` selects a whole assignment — `Config` is advertised
    /// and `Config <- setRefClass(` is selected — which is the outline's
    /// version of the range convention `SymbolInformation` has no
    /// `selectionRange` to settle. Zero occurrences is a disagreement and
    /// several are undecidable, so both stay `None`, and the boundary check
    /// is what keeps `Item` from matching `Item2`.
    fn outline_container(&self, name: &str, selected: &str) -> Option<String> {
        crate::lsp::definition::sole_token(selected, name).map(|_| name.to_string())
    }

    /// The short word for a symbol kind in this server's language, for
    /// result headers and listings: `fn`, `struct`, `def`, `class`…
    fn kind_label(&self, kind: lsp_types::SymbolKind) -> &'static str {
        default_kind_label(kind)
    }
}

/// Language-neutral kind words; adapters override the ones their language
/// spells differently.
pub fn default_kind_label(kind: lsp_types::SymbolKind) -> &'static str {
    use lsp_types::SymbolKind as K;
    match kind {
        K::FILE => "file",
        K::MODULE => "module",
        K::NAMESPACE => "namespace",
        K::PACKAGE => "package",
        K::CLASS => "class",
        K::METHOD => "method",
        K::PROPERTY => "property",
        K::FIELD => "field",
        K::CONSTRUCTOR => "constructor",
        K::ENUM => "enum",
        K::INTERFACE => "interface",
        K::FUNCTION => "function",
        K::VARIABLE => "variable",
        K::CONSTANT => "constant",
        K::STRING | K::NUMBER | K::BOOLEAN | K::ARRAY | K::OBJECT | K::KEY | K::NULL => "value",
        K::ENUM_MEMBER => "enum member",
        K::STRUCT => "struct",
        K::EVENT => "event",
        K::OPERATOR => "operator",
        K::TYPE_PARAMETER => "type parameter",
        _ => "symbol",
    }
}

/// The honest capability set: what this client actually handles. Every
/// server-initiated request it advertises is answered in
/// [`crate::lsp::handlers`].
///
/// `workspace.workspaceFolders` is deliberately *not* advertised. With it,
/// pyright 1.1.403 answered `initialize` and `workspace/symbol` but never a
/// document request (observed with the raw-LSP spike, `docs/spike.md`); the
/// mechanism is not established, and nothing here needs the capability. The
/// folders themselves are still sent in the `initialize` params.
pub fn standard_capabilities(position_encodings: &[&str], experimental: Value) -> Value {
    let mut capabilities = json!({
        "workspace": {
            "configuration": true,
            "symbol": { "symbolKind": { "valueSet": (1..=26).collect::<Vec<_>>() } },
            // Edits come back as results (rename) and are applied by this
            // client: per-document edits, and file renames for a module
            // rename. Creation and deletion are not advertised, so a
            // server must not ask for them.
            "workspaceEdit": { "documentChanges": true, "resourceOperations": ["rename"] },
            // Not offered for dynamic registration. rust-analyzer, measured,
            // registers when it is offered and turns its own watcher off,
            // which leaves it as current as this client's delivery — later
            // than its own, and only at a query boundary. Servers keep their
            // own watchers, and the query boundary's notification is sent
            // on top, unsolicited (`docs/correctness.md`).
            "didChangeWatchedFiles": { "dynamicRegistration": false }
        },
        "textDocument": {
            "synchronization": {
                "dynamicRegistration": false,
                // Full-text changes: the server is sent the whole file when
                // the disk copy differs.
                "didSave": true,
                "change": 1
            },
            "definition": { "linkSupport": false },
            "references": {},
            "implementation": { "linkSupport": false },
            "callHierarchy": {},
            "rename": { "prepareSupport": true },
            "documentSymbol": { "hierarchicalDocumentSymbolSupport": true },
            "hover": { "contentFormat": ["markdown", "plaintext"] },
            "codeAction": {
                "dynamicRegistration": false,
                "dataSupport": true,
                "resolveSupport": { "properties": ["edit"] },
                "codeActionLiteralSupport": {
                    "codeActionKind": {
                        "valueSet": ["", "quickfix", "refactor", "refactor.extract", "refactor.inline", "refactor.rewrite", "source", "source.organizeImports"]
                    }
                }
            }
        },
        "window": { "workDoneProgress": true, "showMessage": {} },
        "general": { "positionEncodings": position_encodings },
    });
    if !experimental.is_null() {
        capabilities["experimental"] = experimental;
    }
    capabilities
}

/// The adapters that are on by default.
///
/// Shared, not owned: a workspace keeps one of these to answer routing
/// questions for a server that is not running, and hands the same value to
/// each launch. `Adapter` is a cheap, immutable descriptor, so sharing it is
/// what "one source of truth about this server" means.
pub fn builtins() -> Vec<Arc<dyn Adapter>> {
    vec![
        Arc::new(rust_analyzer::RustAnalyzer),
        Arc::new(pyright::Pyright),
    ]
}

/// The language id a path's extension maps to, per this list of languages.
pub fn language_for<'a>(languages: &'a [Language], path: &std::path::Path) -> Option<&'a Language> {
    let extension = path.extension()?.to_str()?;
    languages
        .iter()
        .find(|language| language.extensions.iter().any(|e| e == extension))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_capabilities_never_advertise_workspace_folders() {
        let capabilities = standard_capabilities(&["utf-16"], Value::Null);
        assert!(
            capabilities
                .pointer("/workspace/workspaceFolders")
                .is_none()
        );
        assert!(capabilities.get("experimental").is_none());
        assert_eq!(
            capabilities.pointer("/general/positionEncodings"),
            Some(&json!(["utf-16"]))
        );
        let with = standard_capabilities(&["utf-8", "utf-16"], json!({ "x": true }));
        assert_eq!(with.pointer("/experimental/x"), Some(&json!(true)));
    }

    #[test]
    fn an_outline_container_may_be_selected_with_more_than_its_name() {
        struct Generic;
        impl Adapter for Generic {
            fn name(&self) -> &str {
                "generic"
            }
            fn command(&self) -> Vec<String> {
                vec!["generic".to_string()]
            }
            fn languages(&self) -> Vec<Language> {
                vec![Language::new("r", &["R"])]
            }
        }
        let container = |name: &str, selected: &str| Generic.outline_container(name, selected);

        // The name alone, and the name inside a wider selection: R's
        // `languageserver` selects the assignment that defines the class.
        assert_eq!(container("Config", "Config"), Some("Config".to_string()));
        assert_eq!(
            container("Config", "Config <- setRefClass("),
            Some("Config".to_string())
        );

        // A selection that does not name the node is a disagreement, and a
        // longer identifier that merely contains the name is not an
        // occurrence of it.
        assert_eq!(container("Config", "Other"), None);
        assert_eq!(container("Item", "Item2"), None);
        assert_eq!(container("Item", "an_Item"), None);

        // Twice is undecidable rather than the first one.
        assert_eq!(container("Config", "Config <- f(Config)"), None);
    }

    #[test]
    fn language_lookup_is_by_extension() {
        let languages = vec![Language::new("python", &["py", "pyi"])];
        assert_eq!(
            language_for(&languages, std::path::Path::new("/x/a.pyi")).map(|l| l.id.as_str()),
            Some("python")
        );
        assert!(language_for(&languages, std::path::Path::new("/x/a.rs")).is_none());
        assert!(language_for(&languages, std::path::Path::new("/x/Makefile")).is_none());
    }
}
