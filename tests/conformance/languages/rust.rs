//! Rust: one candidate, which is the finding.
//!
//! The dataset records `rust-analyzer` in all ten editor inventories and
//! nothing else that navigates — `rls` is source-only in lsp-mode with no
//! argv recorded, and `bacon-ls` serves diagnostics. So this column is a
//! **regression baseline for the server structural ships against**, not a
//! comparison: a `no` here is a change in rust-analyzer or a defect in
//! `src/`, and never a choice between servers.

use crate::probe::{Candidate, Language, Probe, rename_preview, show, uses};
use crate::report::Outcome;

pub const RUST: Language = Language {
    name: "rust",
    fixture: "rust",
    extensions: &[],
    // A second file, so cross-file references have somewhere to be. It is
    // `src/main.rs` rather than another module because Cargo picks that up
    // as a binary target on its own: a new module would have to be declared
    // in `lib.rs`, and the smoke tests assert that file's line numbers.
    extra_files: &[(
        "src/main.rs",
        "use structural_spike_fixture::{Mode, choose};\n\
         \n\
         fn main() {\n    \
             println!(\"{}\", choose(Mode::Fast));\n\
         }\n",
    )],
    candidates: CANDIDATES,
    probes: PROBES,
};

const CANDIDATES: &[Candidate] = &[Candidate {
    name: "rust-analyzer",
    argv: &["rust-analyzer"],
    version_argv: &["rust-analyzer", "--version"],
    builtin: true,
    settings: "",
    extra_config: "",
}];

const PROBES: &[Probe] = &[
    Probe {
        name: "bare name",
        needs: "workspace/symbol",
        run: |state| {
            let answer = show(state, Some("choose"), None, None);
            Outcome::judge(&answer, |text| {
                text.contains("lib.rs") && text.contains("pub fn choose")
            })
        },
    },
    Probe {
        name: "file-scoped name",
        needs: "documentSymbol",
        run: |state| {
            let answer = show(state, Some("identity"), Some("src/lib.rs"), None);
            Outcome::judge(&answer, |text| text.contains("pub fn identity"))
        },
    },
    Probe {
        name: "qualified name",
        needs: "outline containers",
        run: |state| {
            let answer = show(state, Some("Config::doubled"), None, None);
            Outcome::judge(&answer, |text| text.contains("pub fn doubled"))
        },
    },
    Probe {
        name: "qualified name, in file",
        needs: "outline nesting",
        run: |state| {
            let answer = show(state, Some("Config::doubled"), Some("src/lib.rs"), None);
            Outcome::judge(&answer, |text| text.contains("pub fn doubled"))
        },
    },
    Probe {
        name: "cross-file uses",
        needs: "references",
        run: |state| {
            // `choose` is defined and called in lib.rs and called again
            // from main.rs: a server that reports only the first file has
            // not searched the workspace.
            let answer = uses(state, "choose");
            Outcome::judge(&answer, |text| text.contains("main.rs"))
        },
    },
    Probe {
        name: "anchor to a local",
        needs: "definition at a position",
        run: |state| {
            let answer = show(
                state,
                None,
                Some("src/lib.rs"),
                Some(("let count = config.doubled();", "doubled")),
            );
            Outcome::judge(&answer, |text| text.contains("pub fn doubled"))
        },
    },
    Probe {
        name: "rename preview",
        needs: "rename WorkspaceEdit",
        run: |state| {
            let answer = rename_preview(state, "choose", "pick");
            Outcome::judge(&answer, |text| {
                text.contains("lib.rs") && text.contains("main.rs")
            })
        },
    },
    Probe {
        name: "bare name, warm",
        needs: "workspace/symbol after a file is open",
        run: |state| {
            let answer = show(state, Some("choose"), None, None);
            Outcome::judge(&answer, |text| {
                text.contains("lib.rs") && text.contains("pub fn choose")
            })
        },
    },
];
