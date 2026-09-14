//! pyright: no extensions, no readiness signal beyond standard progress, the
//! standard capability profile (which already omits the one capability the
//! spike found it could not live with; see [`super::standard_capabilities`]).

use super::{Adapter, Language, default_kind_label};
use lsp_types::SymbolKind;

pub struct Pyright;

impl Adapter for Pyright {
    fn name(&self) -> &str {
        "pyright"
    }

    fn command(&self) -> Vec<String> {
        vec!["pyright-langserver".to_string(), "--stdio".to_string()]
    }

    fn install_hint(&self) -> String {
        "`npm install -g pyright` (or `pip install pyright`), and ensure `pyright-langserver` is on PATH"
            .to_string()
    }

    fn languages(&self) -> Vec<Language> {
        vec![Language::new("python", &["py", "pyi"])]
    }

    fn kind_label(&self, kind: SymbolKind) -> &'static str {
        match kind {
            SymbolKind::FUNCTION | SymbolKind::METHOD => "def",
            other => default_kind_label(other),
        }
    }
}
