//! A server named in `structural.toml`: either a wholly generic command with
//! its languages, or an override of a built-in's command or backstop.

use super::{Adapter, Language, ReadinessPolicy};
use serde_json::Value;
use std::sync::Arc;
use std::time::Duration;

pub struct Configured {
    pub name: String,
    pub command: Vec<String>,
    pub languages: Vec<Language>,
    pub backstop: Duration,
    /// The built-in this entry overrides, when its name matches one: the
    /// server-specific knowledge (policy, capabilities, query rewriting)
    /// still comes from there. `None` for a wholly user-defined server.
    pub base: Option<Arc<dyn Adapter>>,
}

impl Adapter for Configured {
    fn name(&self) -> &str {
        &self.name
    }

    fn command(&self) -> Vec<String> {
        self.command.clone()
    }

    fn install_hint(&self) -> String {
        match &self.base {
            Some(base) => base.install_hint(),
            None => format!(
                "`{}` is configured in structural.toml; install it and ensure it is on PATH",
                self.command[0]
            ),
        }
    }

    fn languages(&self) -> Vec<Language> {
        self.languages.clone()
    }

    fn initialization_options(&self) -> Value {
        match &self.base {
            Some(base) => base.initialization_options(),
            None => serde_json::json!({}),
        }
    }

    fn experimental_capabilities(&self) -> Value {
        match &self.base {
            Some(base) => base.experimental_capabilities(),
            None => Value::Null,
        }
    }

    fn position_encodings(&self) -> Vec<&'static str> {
        match &self.base {
            Some(base) => base.position_encodings(),
            None => vec!["utf-16"],
        }
    }

    fn client_capabilities(&self) -> Value {
        match &self.base {
            Some(base) => base.client_capabilities(),
            None => super::standard_capabilities(&self.position_encodings(), Value::Null),
        }
    }

    fn readiness_policy(&self) -> Box<dyn ReadinessPolicy> {
        match &self.base {
            Some(base) => base.readiness_policy(),
            None => Box::new(super::StandardIdle),
        }
    }

    fn symbol_query(&self, query: &str) -> String {
        match &self.base {
            Some(base) => base.symbol_query(query),
            None => query.to_string(),
        }
    }

    fn backstop(&self) -> Duration {
        self.backstop
    }

    fn kind_label(&self, kind: lsp_types::SymbolKind) -> &'static str {
        match &self.base {
            Some(base) => base.kind_label(kind),
            None => super::default_kind_label(kind),
        }
    }

    fn rename_note(&self, old_name: &str) -> Option<String> {
        match &self.base {
            Some(base) => base.rename_note(old_name),
            None => None,
        }
    }

    fn extra_views(&self) -> Vec<&'static str> {
        match &self.base {
            Some(base) => base.extra_views(),
            None => Vec::new(),
        }
    }

    fn extra_view_request(
        &self,
        view: &str,
        uri: &str,
        position: lsp_types::Position,
    ) -> Option<(String, Value)> {
        match &self.base {
            Some(base) => base.extra_view_request(view, uri, position),
            None => None,
        }
    }

    fn render_extra_view(
        &self,
        view: &str,
        result: &Value,
        root: &std::path::Path,
    ) -> anyhow::Result<String> {
        match &self.base {
            Some(base) => base.render_extra_view(view, result, root),
            None => anyhow::bail!("{} has no view named `{view}`", self.name()),
        }
    }

    fn hover_params(&self, uri: &str, range: lsp_types::Range) -> Value {
        match &self.base {
            Some(base) => base.hover_params(uri, range),
            None => serde_json::json!({ "textDocument": { "uri": uri }, "position": range.start }),
        }
    }

    fn reference_aliases(&self) -> Vec<&'static str> {
        match &self.base {
            Some(base) => base.reference_aliases(),
            None => Vec::new(),
        }
    }

    fn outline_container(&self, name: &str, selected: &str) -> Option<String> {
        match &self.base {
            Some(base) => base.outline_container(name, selected),
            None => (name == selected).then(|| name.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::rust_analyzer::RustAnalyzer;
    use super::*;

    fn configured(base: Option<Arc<dyn Adapter>>) -> Configured {
        Configured {
            name: "rust-analyzer".into(),
            command: vec!["mock".into()],
            languages: vec![Language::new("rust", &["rs"])],
            backstop: Duration::from_secs(60),
            base,
        }
    }

    #[test]
    fn an_override_of_a_builtin_keeps_its_rename_note_and_a_generic_server_has_none() {
        assert!(
            configured(Some(Arc::new(RustAnalyzer)))
                .rename_note("choose")
                .is_some()
        );
        assert!(configured(None).rename_note("choose").is_none());
    }

    /// Every method where rust-analyzer answers something other than the
    /// trait default, asked of an override of it.
    ///
    /// A delegation is added by hand per method, so one can be forgotten,
    /// and forgetting is silent: the trait default answers instead, and the
    /// caller sees a server that has quietly lost what the built-in knows.
    /// `outline_container` was missing this way, and a file-scoped
    /// `Config::doubled` against an overridden rust-analyzer failed with a
    /// message blaming the outline for not describing the file — because
    /// the default compares the ancestor's advertised name (`impl Config`)
    /// to the text at its range (`Config`) and calls the difference a
    /// disagreement (docs/servers.md).
    ///
    /// Adding a method to `Adapter` that a built-in overrides means adding
    /// a case here.
    #[test]
    fn an_override_answers_every_server_specific_question_as_the_builtin_does() {
        let builtin = RustAnalyzer;
        let over = configured(Some(Arc::new(RustAnalyzer)));
        let position = lsp_types::Position::new(1, 0);
        let range = lsp_types::Range::new(position, position);

        assert_eq!(
            over.outline_container("impl Config", "Config"),
            builtin.outline_container("impl Config", "Config"),
        );
        assert_eq!(over.extra_views(), builtin.extra_views());
        for view in builtin.extra_views() {
            assert_eq!(
                over.extra_view_request(view, "file:///x.rs", position),
                builtin.extra_view_request(view, "file:///x.rs", position),
                "the request behind `{view}`"
            );
        }
        assert_eq!(
            over.hover_params("file:///x.rs", range),
            builtin.hover_params("file:///x.rs", range),
        );
        assert_eq!(over.reference_aliases(), builtin.reference_aliases());
        assert_eq!(over.symbol_query("Config"), builtin.symbol_query("Config"));
        assert_eq!(
            over.initialization_options(),
            builtin.initialization_options()
        );
        assert_eq!(
            over.experimental_capabilities(),
            builtin.experimental_capabilities()
        );
        assert_eq!(over.position_encodings(), builtin.position_encodings());
        assert_eq!(over.client_capabilities(), builtin.client_capabilities());
        assert_eq!(over.install_hint(), builtin.install_hint());
        assert_eq!(
            over.kind_label(lsp_types::SymbolKind::FUNCTION),
            builtin.kind_label(lsp_types::SymbolKind::FUNCTION),
        );
        // `render_extra_view` is reached with a server's own result shape,
        // so it is asserted by the view tests rather than here; what this
        // case can say is that the override does not answer "no such view"
        // for a view the built-in offers.
        for view in builtin.extra_views() {
            let rendered = over.render_extra_view(view, &Value::Null, std::path::Path::new("/"));
            let message = rendered
                .map(|_| String::new())
                .unwrap_or_else(|e| e.to_string());
            assert!(
                !message.contains("has no view named"),
                "`{view}` is the built-in's view, and the override refused it"
            );
        }
    }
}
