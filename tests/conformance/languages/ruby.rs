//! Ruby: three candidates — `solargraph`, `ruby-lsp`, and `typeprof --lsp`.
//! The last is a type-inference tool with an LSP interface rather than a
//! general-purpose language server. It is expected to answer few of these
//! probes; a `no` here is the tool's own catalogue fact, not a fixture
//! defect, so the fixture is not contorted to avoid one.

use crate::probe::{Candidate, Language, Probe, rename_preview, show, uses};
use crate::report::Outcome;

pub const RUBY: Language = Language {
    name: "ruby",
    fixture: "ruby",
    extensions: &[],
    extra_files: &[],
    generated_files: &[],
    candidates: CANDIDATES,
    probes: PROBES,
};

const CANDIDATES: &[Candidate] = &[
    Candidate {
        name: "solargraph",
        argv: &["solargraph", "stdio"],
        version_argv: &["solargraph", "--version"],
        builtin: false,
        settings: "",
        extra_config: "",
    },
    Candidate {
        name: "ruby-lsp",
        argv: &["ruby-lsp"],
        version_argv: &["ruby-lsp", "--version"],
        builtin: false,
        settings: "",
        extra_config: "",
    },
    Candidate {
        name: "typeprof",
        argv: &["typeprof", "--lsp", "--stdio"],
        version_argv: &["typeprof", "--version"],
        builtin: false,
        settings: "",
        extra_config: "",
    },
];

const PROBES: &[Probe] = &[
    Probe {
        name: "bare name",
        needs: "workspace/symbol",
        run: |state| {
            let answer = show(state, Some("choose"), None, None);
            Outcome::judge(&answer, |text| {
                text.contains("helper.rb") && text.contains("def choose")
            })
        },
    },
    Probe {
        name: "file-scoped name",
        needs: "documentSymbol",
        run: |state| {
            let answer = show(state, Some("identity"), Some("helper.rb"), None);
            Outcome::judge(&answer, |text| text.contains("def identity"))
        },
    },
    Probe {
        name: "qualified name",
        needs: "outline containers",
        run: |state| {
            let answer = show(state, Some("Config.doubled"), None, None);
            Outcome::judge(&answer, |text| text.contains("def doubled"))
        },
    },
    Probe {
        // The same qualifier, resolved from the file's own outline instead
        // of the index, as in python.rs.
        name: "qualified name, in file",
        needs: "outline nesting",
        run: |state| {
            let answer = show(state, Some("Config.doubled"), Some("helper.rb"), None);
            Outcome::judge(&answer, |text| text.contains("def doubled"))
        },
    },
    Probe {
        name: "cross-file uses",
        needs: "references",
        run: |state| {
            let answer = uses(state, "choose");
            // The definition is in helper.rb and the only call is in
            // main.rb: a server that reports just the definition has not
            // searched the project.
            Outcome::judge(&answer, |text| text.contains("main.rb"))
        },
    },
    Probe {
        name: "anchor to a local",
        needs: "definition at a position",
        run: |state| {
            let answer = show(
                state,
                None,
                Some("main.rb"),
                Some(("count = config.doubled", "doubled")),
            );
            Outcome::judge(&answer, |text| text.contains("def doubled"))
        },
    },
    Probe {
        name: "rename preview",
        needs: "rename WorkspaceEdit",
        run: |state| {
            let answer = rename_preview(state, "choose", "pick");
            // Two files, because the call site is in the other one.
            Outcome::judge(&answer, |text| {
                text.contains("helper.rb") && text.contains("main.rb")
            })
        },
    },
    Probe {
        name: "bare name, warm",
        needs: "workspace/symbol after a file is open",
        run: |state| {
            let answer = show(state, Some("choose"), None, None);
            Outcome::judge(&answer, |text| {
                text.contains("helper.rb") && text.contains("def choose")
            })
        },
    },
];
