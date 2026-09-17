//! Gleam: the `gleam lsp` subcommand of the `gleam` compiler, and the
//! fixture whose language has no type to qualify a name with.
//!
//! Like Erlang, the container of a function here is its **module**, not a
//! type: a qualified call is written `module.function` — `helper.choose`
//! in `src/main.gleam`. Unlike Erlang, `.` is one of the separators
//! structural already splits a qualified `symbol` on
//! (`src/tools/resolution/qualify.rs`), so `helper.choose` reaches the
//! qualifier filter exactly as the user would spell it — no `::`
//! substitution needed. Whether `gleam lsp` reports the module as the
//! container of `choose` is still what the qualified-name probes measure;
//! a `no` here is the finding, not a harness fault.
//!
//! `gleam lsp` answers questions about a package the compiler has already
//! built at least once — it wants a `build/` directory alongside the
//! sources, which only `gleam build` (or `gleam check`) produces. If this
//! column reads all `no`, that is the
//! `backlog/generic-baseline/answering-is-availability.md` case — a server
//! that never became ready to answer — not evidence the questions
//! themselves are wrong.

use crate::probe::{Candidate, Language, Probe, rename_preview, show, uses};
use crate::report::Outcome;

pub const GLEAM: Language = Language {
    name: "gleam",
    fixture: "gleam",
    extensions: &["gleam"],
    extra_files: &[],
    candidates: CANDIDATES,
    probes: PROBES,
};

const CANDIDATES: &[Candidate] = &[Candidate {
    name: "gleam",
    argv: &["gleam", "lsp"],
    version_argv: &["gleam", "--version"],
    builtin: false,
    settings: "",
}];

const PROBES: &[Probe] = &[
    Probe {
        name: "bare name",
        needs: "workspace/symbol",
        run: |state| {
            let answer = show(state, Some("choose"), None, None);
            Outcome::judge(&answer, |text| {
                text.contains("helper.gleam") && text.contains("pub fn choose(flag: Bool)")
            })
        },
    },
    Probe {
        name: "file-scoped name",
        needs: "documentSymbol",
        run: |state| {
            let answer = show(state, Some("identity"), Some("src/helper.gleam"), None);
            Outcome::judge(&answer, |text| text.contains("pub fn identity(value: Int)"))
        },
    },
    Probe {
        name: "qualified name",
        needs: "outline containers",
        run: |state| {
            // The container is the module `helper`; `.` is already one of
            // the separators `symbol` splits on, so this is spelled the
            // way a Gleam programmer would write the call.
            let answer = show(state, Some("helper.choose"), None, None);
            Outcome::judge(&answer, |text| text.contains("pub fn choose(flag: Bool)"))
        },
    },
    Probe {
        name: "qualified name, in file",
        needs: "outline nesting",
        run: |state| {
            let answer = show(state, Some("helper.choose"), Some("src/helper.gleam"), None);
            Outcome::judge(&answer, |text| text.contains("pub fn choose(flag: Bool)"))
        },
    },
    Probe {
        name: "cross-file uses",
        needs: "references",
        run: |state| {
            // `choose` is declared in src/helper.gleam and called from
            // src/main.gleam.
            let answer = uses(state, "choose");
            Outcome::judge(&answer, |text| text.contains("main.gleam"))
        },
    },
    Probe {
        name: "anchor to a local",
        needs: "definition at a position",
        run: |state| {
            let answer = show(
                state,
                None,
                Some("src/main.gleam"),
                Some(("let offset = helper.choose(True)", "choose")),
            );
            Outcome::judge(&answer, |text| text.contains("pub fn choose(flag: Bool)"))
        },
    },
    Probe {
        name: "rename preview",
        needs: "rename WorkspaceEdit",
        run: |state| {
            let answer = rename_preview(state, "choose", "pick");
            Outcome::judge(&answer, |text| {
                text.contains("helper.gleam") && text.contains("main.gleam")
            })
        },
    },
    Probe {
        name: "bare name, warm",
        needs: "workspace/symbol after a file is open",
        run: |state| {
            let answer = show(state, Some("choose"), None, None);
            Outcome::judge(&answer, |text| {
                text.contains("helper.gleam") && text.contains("pub fn choose(flag: Bool)")
            })
        },
    },
];
