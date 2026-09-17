//! Go: `gopls` in all ten editor inventories, with
//! `golangci-lint-langserver` recorded as an add-on rather than an
//! alternative. Like Rust, this column is a regression baseline for one
//! server and not a comparison.

use crate::probe::{Candidate, Language, Probe, rename_preview, show, uses};
use crate::report::Outcome;

pub const GO: Language = Language {
    name: "go",
    fixture: "go",
    // The shared Go fixture has no method to qualify, and adding one to
    // `greet.go` would move the line numbers `configured.rs` asserts. A
    // file of its own adds the symbol without touching that file.
    extra_files: &[(
        "config.go",
        "package scratch\n\
         \n\
         // Config carries a limit.\n\
         type Config struct {\n\t\
             Limit int\n\
         }\n\
         \n\
         // Doubled is twice the limit.\n\
         func (c Config) Doubled() int {\n\t\
             return c.Limit * 2\n\
         }\n",
    )],
    candidates: CANDIDATES,
    probes: PROBES,
};

const CANDIDATES: &[Candidate] = &[Candidate {
    name: "gopls",
    argv: &["gopls"],
    version_argv: &["gopls", "version"],
    builtin: false,
}];

const PROBES: &[Probe] = &[
    Probe {
        name: "bare name",
        needs: "workspace/symbol",
        run: |state| {
            let answer = show(state, Some("Greet"), None, None);
            Outcome::judge(&answer, |text| {
                text.contains("greet.go") && text.contains("func Greet")
            })
        },
    },
    Probe {
        name: "file-scoped name",
        needs: "documentSymbol",
        run: |state| {
            let answer = show(state, Some("Run"), Some("main.go"), None);
            Outcome::judge(&answer, |text| text.contains("func Run"))
        },
    },
    Probe {
        name: "qualified name",
        needs: "outline containers",
        run: |state| {
            let answer = show(state, Some("Config.Doubled"), None, None);
            Outcome::judge(&answer, |text| text.contains("func (c Config) Doubled"))
        },
    },
    Probe {
        name: "qualified name, in file",
        needs: "outline nesting",
        run: |state| {
            let answer = show(state, Some("Config.Doubled"), Some("config.go"), None);
            Outcome::judge(&answer, |text| text.contains("func (c Config) Doubled"))
        },
    },
    Probe {
        name: "cross-file uses",
        needs: "references",
        run: |state| {
            // `Greet` is declared in greet.go and called from main.go and
            // greet_test.go.
            let answer = uses(state, "Greet");
            Outcome::judge(&answer, |text| text.contains("main.go"))
        },
    },
    Probe {
        name: "anchor to a local",
        needs: "definition at a position",
        run: |state| {
            let answer = show(
                state,
                None,
                Some("main.go"),
                Some(("return Greet(\"world\")", "Greet")),
            );
            Outcome::judge(&answer, |text| text.contains("func Greet"))
        },
    },
    Probe {
        name: "rename preview",
        needs: "rename WorkspaceEdit",
        run: |state| {
            let answer = rename_preview(state, "Greet", "Hello");
            Outcome::judge(&answer, |text| {
                text.contains("greet.go") && text.contains("main.go")
            })
        },
    },
    Probe {
        name: "bare name, warm",
        needs: "workspace/symbol after a file is open",
        run: |state| {
            let answer = show(state, Some("Greet"), None, None);
            Outcome::judge(&answer, |text| {
                text.contains("greet.go") && text.contains("func Greet")
            })
        },
    },
];
