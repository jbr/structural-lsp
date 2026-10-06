//! Elm: `elm-language-server`, and another fixture whose container is a
//! **module** rather than a type — like Erlang (`erlang.rs`), but here the
//! qualifier separator is `.`, one of the three structural already splits on
//! (`src/tools/resolution/qualify.rs`), so the qualified-name probes need no
//! workaround: `Helper.choose` parses as the bare name `choose` filtered by
//! the container `Helper`, exactly the spelling a caller would write anyway.
//! Whether `elm-language-server` reports the module as the container is what
//! those two probes measure.
//!
//! `elm-language-server` needs more than a checkout of this fixture to
//! answer anything: an `elm` compiler on `PATH` and a populated
//! `~/.elm` package cache (it shells out to the compiler for diagnostics and
//! reads the cache for `elm/core`). A column that reads `no` straight down
//! because packages were never downloaded in the probe environment is the
//! `backlog/generic-baseline/answering-is-availability.md` case — the server
//! never got to try — not a finding about the server's LSP support.

use crate::probe::{Candidate, Language, Probe, rename_preview, show, uses};
use crate::report::Outcome;

pub const ELM: Language = Language {
    name: "elm",
    fixture: "elm",
    extensions: &["elm"],
    extra_files: &[],
    generated_files: &[],
    candidates: CANDIDATES,
    probes: PROBES,
};

const CANDIDATES: &[Candidate] = &[Candidate {
    name: "elm-language-server",
    argv: &["elm-language-server"],
    version_argv: &["elm-language-server", "--version"],
    builtin: false,
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
                text.contains("Helper.elm") && text.contains("choose : Bool -> Int")
            })
        },
    },
    Probe {
        name: "file-scoped name",
        needs: "documentSymbol",
        run: |state| {
            let answer = show(state, Some("identity"), Some("src/Helper.elm"), None);
            Outcome::judge(&answer, |text| text.contains("identity : Int -> Int"))
        },
    },
    Probe {
        name: "qualified name",
        needs: "outline containers",
        run: |state| {
            // The container is the module `Helper`; `.` is already a
            // separator structural splits on, so this is the plain spelling.
            let answer = show(state, Some("Helper.choose"), None, None);
            Outcome::judge(&answer, |text| text.contains("choose : Bool -> Int"))
        },
    },
    Probe {
        name: "qualified name, in file",
        needs: "outline nesting",
        run: |state| {
            let answer = show(state, Some("Helper.choose"), Some("src/Helper.elm"), None);
            Outcome::judge(&answer, |text| text.contains("choose : Bool -> Int"))
        },
    },
    Probe {
        name: "cross-file uses",
        needs: "references",
        run: |state| {
            // `choose` is declared in src/Helper.elm and called from
            // src/Main.elm.
            let answer = uses(state, "choose");
            Outcome::judge(&answer, |text| text.contains("Main.elm"))
        },
    },
    Probe {
        name: "anchor to a local",
        needs: "definition at a position",
        run: |state| {
            let answer = show(
                state,
                None,
                Some("src/Main.elm"),
                Some(("Helper.choose True", "choose")),
            );
            Outcome::judge(&answer, |text| text.contains("choose : Bool -> Int"))
        },
    },
    Probe {
        name: "rename preview",
        needs: "rename WorkspaceEdit",
        run: |state| {
            let answer = rename_preview(state, "choose", "pick");
            Outcome::judge(&answer, |text| {
                text.contains("Helper.elm") && text.contains("Main.elm")
            })
        },
    },
    Probe {
        name: "bare name, warm",
        needs: "workspace/symbol after a file is open",
        run: |state| {
            let answer = show(state, Some("choose"), None, None);
            Outcome::judge(&answer, |text| {
                text.contains("Helper.elm") && text.contains("choose : Bool -> Int")
            })
        },
    },
];
