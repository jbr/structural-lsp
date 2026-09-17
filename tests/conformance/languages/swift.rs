//! Swift: `sourcekit-lsp`, and the one fixture here whose language writes
//! argument labels into a declaration's name.
//!
//! Swift spells a function's name with its argument labels —
//! `choose(flag:)`, `identity(value:)`, `Config.doubled()` — and
//! sourcekit-lsp reports symbols that way in both `workspace/symbol` and
//! `documentSymbol`. structural resolves only exact names
//! (`docs/correctness.md`), so a probe asking for `choose` is asking for a
//! name the server does not hold, and the expected answer is "no such
//! symbol". The probes below still ask the bare names, because that is the
//! question the other five languages are asked and the difference between
//! the columns is the finding: a `no` here localizes to naming, not to a
//! missing capability. The two probes that reach a symbol by position
//! rather than by name — "anchor to a local", and the `path`-scoped ones if
//! the outline is consulted by range — are the ones that can answer without
//! crossing that gap.
//!
//! The SwiftPM manifest is in the fixture because sourcekit-lsp resolves a
//! module through `Package.swift`; without it there is no target, no module
//! name, and what would be measured is the missing manifest rather than the
//! server. The second file is `Caller.swift` and not `main.swift` because
//! SwiftPM gives a file named `main.swift` top-level-code semantics, which
//! is a different compilation shape from the one the other fixtures use.

use crate::probe::{Candidate, Language, Probe, rename_preview, show, uses};
use crate::report::Outcome;

pub const SWIFT: Language = Language {
    name: "swift",
    fixture: "swift",
    // "swift" is not one of the eleven language ids `src/config.rs` knows,
    // so the extension table has to be spelled out here.
    extensions: &["swift"],
    extra_files: &[],
    candidates: CANDIDATES,
    probes: PROBES,
};

const CANDIDATES: &[Candidate] = &[Candidate {
    name: "sourcekit-lsp",
    argv: &["sourcekit-lsp"],
    version_argv: &["sourcekit-lsp", "--version"],
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
                text.contains("Helper.swift") && text.contains("func choose")
            })
        },
    },
    Probe {
        name: "file-scoped name",
        needs: "documentSymbol",
        run: |state| {
            let answer = show(
                state,
                Some("identity"),
                Some("Sources/Scratch/Helper.swift"),
                None,
            );
            Outcome::judge(&answer, |text| text.contains("func identity"))
        },
    },
    Probe {
        name: "qualified name",
        needs: "outline containers",
        run: |state| {
            let answer = show(state, Some("Config.doubled"), None, None);
            Outcome::judge(&answer, |text| text.contains("func doubled"))
        },
    },
    Probe {
        name: "qualified name, in file",
        needs: "outline nesting",
        run: |state| {
            let answer = show(
                state,
                Some("Config.doubled"),
                Some("Sources/Scratch/Helper.swift"),
                None,
            );
            Outcome::judge(&answer, |text| text.contains("func doubled"))
        },
    },
    Probe {
        name: "cross-file uses",
        needs: "references",
        run: |state| {
            // `choose` is declared in Helper.swift and called from
            // Caller.swift.
            let answer = uses(state, "choose");
            Outcome::judge(&answer, |text| text.contains("Caller.swift"))
        },
    },
    Probe {
        name: "anchor to a local",
        needs: "definition at a position",
        run: |state| {
            let answer = show(
                state,
                None,
                Some("Sources/Scratch/Caller.swift"),
                Some(("let offset = choose(flag: true)", "choose")),
            );
            Outcome::judge(&answer, |text| text.contains("func choose"))
        },
    },
    Probe {
        name: "rename preview",
        needs: "rename WorkspaceEdit",
        run: |state| {
            let answer = rename_preview(state, "choose", "pick");
            Outcome::judge(&answer, |text| {
                text.contains("Helper.swift") && text.contains("Caller.swift")
            })
        },
    },
    Probe {
        name: "bare name, warm",
        needs: "workspace/symbol after a file is open",
        run: |state| {
            let answer = show(state, Some("choose"), None, None);
            Outcome::judge(&answer, |text| {
                text.contains("Helper.swift") && text.contains("func choose")
            })
        },
    },
];
