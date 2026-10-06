//! R: one candidate, `languageserver`, run as an R package inside an R
//! process rather than as a compiled binary — the reason to probe it at all
//! is that it may emphasise a different subset of the protocol than the
//! compiled servers the other columns measure.
//!
//! R has three ways to spell "a type with a method" and the fixture needs
//! to pick one for the qualified-name probes (`Config.doubled`). The three,
//! and why Reference Classes (R5, `setRefClass`) were chosen:
//!
//! - **S3**: a plain function named `doubled.Config`, dispatch by naming
//!   convention. The class and the method are not lexically nested — the
//!   whole relationship is the string in the function's own name — so an
//!   outline that reports containers has nothing structural to find, and
//!   the dotted order is backwards from the `Container.symbol` convention
//!   the other languages use.
//! - **S4**: `setMethod("doubled", "Config", function(object) ...)`
//!   registers the method by passing the class name as a string argument
//!   to a call; the method's function literal is not textually inside a
//!   `Config` definition anywhere.
//! - **R5 / Reference Classes**: `setRefClass("Config", methods = list(doubled
//!   = function() ...))` puts the method's function literal lexically
//!   inside the same call that names the class, as an entry of the
//!   `methods` list. It is the only one of the three where a source-based
//!   outline has an actual nesting to report.
//!
//! This is a guess about `languageserver`'s outline, not a verified fact:
//! `languageserver` parses with R's own tokenizer rather than evaluating
//! the `setRefClass` call, so whether it recognises the `methods = list(...)`
//! shape as a container at all is exactly what probes 3 and 4 below are
//! for. If it does not, the honest reading is that R has no shape
//! `languageserver` reports as a qualified name's container, and the `no`
//! on both probes is that finding.
//!
//! `r` is not one of the eleven language ids `src/config.rs` knows
//! (`default_extensions`), so `Language::extensions` spells out `r` and
//! `R`. `adapter::language_for` matches a path's extension by `==` against
//! that list with no case-folding (`src/lsp/adapter.rs`), so the match is
//! case-sensitive; the fixture files are named `helper.R` and `main.R`,
//! the conventional case for R scripts, and rely on `R` being in the
//! extensions list rather than on any case-insensitive fallback.
//!
//! `fixtures/r/DESCRIPTION.in` is a minimal package description added on
//! the assumption that `languageserver` (via `rprojroot`) walks upward from
//! an open file looking for a project marker to decide what "the project"
//! is for workspace-wide indexing; without a marker inside the fixture
//! tree, that walk could leave the tree entirely and land on some ancestor
//! directory. Unverified — the DESCRIPTION file costs nothing if the
//! server does not do this.

use crate::probe::{Candidate, Language, Probe, rename_preview, show, uses};
use crate::report::Outcome;

pub const R: Language = Language {
    name: "r",
    fixture: "r",
    extensions: &["r", "R"],
    extra_files: &[],
    generated_files: &[],
    candidates: CANDIDATES,
    probes: PROBES,
};

const CANDIDATES: &[Candidate] = &[Candidate {
    name: "R",
    argv: &["R", "--slave", "-e", "languageserver::run()"],
    version_argv: &["R", "--version"],
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
                text.contains("helper.R") && text.contains("choose <- function(")
            })
        },
    },
    Probe {
        name: "file-scoped name",
        needs: "documentSymbol",
        run: |state| {
            let answer = show(state, Some("identity"), Some("helper.R"), None);
            Outcome::judge(&answer, |text| text.contains("identity <- function("))
        },
    },
    Probe {
        name: "qualified name",
        needs: "outline containers",
        run: |state| {
            let answer = show(state, Some("Config.doubled"), None, None);
            Outcome::judge(&answer, |text| text.contains("doubled = function("))
        },
    },
    Probe {
        name: "qualified name, in file",
        needs: "outline nesting",
        run: |state| {
            let answer = show(state, Some("Config.doubled"), Some("helper.R"), None);
            Outcome::judge(&answer, |text| text.contains("doubled = function("))
        },
    },
    Probe {
        name: "cross-file uses",
        needs: "references",
        run: |state| {
            // `choose` is declared in helper.R and called from main.R.
            let answer = uses(state, "choose");
            Outcome::judge(&answer, |text| text.contains("main.R"))
        },
    },
    Probe {
        name: "anchor to a local",
        needs: "definition at a position",
        run: |state| {
            let answer = show(
                state,
                None,
                Some("main.R"),
                Some(("offset <- choose(TRUE)", "choose")),
            );
            Outcome::judge(&answer, |text| text.contains("choose <- function("))
        },
    },
    Probe {
        name: "rename preview",
        needs: "rename WorkspaceEdit",
        run: |state| {
            let answer = rename_preview(state, "choose", "pick");
            Outcome::judge(&answer, |text| {
                text.contains("helper.R") && text.contains("main.R")
            })
        },
    },
    Probe {
        name: "bare name, warm",
        needs: "workspace/symbol after a file is open",
        run: |state| {
            let answer = show(state, Some("choose"), None, None);
            Outcome::judge(&answer, |text| {
                text.contains("helper.R") && text.contains("choose <- function(")
            })
        },
    },
];
