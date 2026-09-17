//! Bash: `bash-language-server`, built on a tree-sitter parser rather than a
//! compiler or type checker, which is the reason to measure which subset of
//! the protocol it emphasises.
//!
//! Bash has no types, no methods and no modules — a function's only
//! container is the file it is defined in. The qualified-name probes have
//! no Bash spelling at all, not even the nearest-meaningful-question
//! rewrite Erlang's module gets: there is no name a `symbol` could be
//! qualified *by*. They are omitted here because the language has no
//! container to ask about, not because bash-language-server fails to
//! answer them.
//!
//! `choose` is declared in `helper.sh` and called from `main.sh`, which
//! `source`s it, so cross-file references and rename have somewhere to be
//! found. `identity` exists only to give the file-scoped probe a second
//! name to ask for.

use crate::probe::{Candidate, Language, Probe, rename_preview, show, uses};
use crate::report::Outcome;

pub const BASH: Language = Language {
    name: "bash",
    fixture: "bash",
    // "bash" is not one of the eleven language ids `src/config.rs` knows.
    extensions: &["sh", "bash"],
    extra_files: &[],
    candidates: CANDIDATES,
    probes: PROBES,
};

const CANDIDATES: &[Candidate] = &[Candidate {
    name: "bash-language-server",
    argv: &["bash-language-server", "start"],
    version_argv: &["bash-language-server", "--version"],
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
                text.contains("helper.sh") && text.contains("choose()")
            })
        },
    },
    Probe {
        name: "file-scoped name",
        needs: "documentSymbol",
        run: |state| {
            let answer = show(state, Some("identity"), Some("helper.sh"), None);
            Outcome::judge(&answer, |text| text.contains("identity()"))
        },
    },
    Probe {
        name: "cross-file uses",
        needs: "references",
        run: |state| {
            // `choose` is declared in helper.sh and called from main.sh,
            // which reaches it with `source`.
            let answer = uses(state, "choose");
            Outcome::judge(&answer, |text| text.contains("main.sh"))
        },
    },
    Probe {
        name: "anchor to a local",
        needs: "definition at a position",
        run: |state| {
            let answer = show(
                state,
                None,
                Some("main.sh"),
                Some(("result=$(choose true \"yes\" \"no\")", "choose")),
            );
            Outcome::judge(&answer, |text| text.contains("choose()"))
        },
    },
    Probe {
        name: "rename preview",
        needs: "rename WorkspaceEdit",
        run: |state| {
            let answer = rename_preview(state, "choose", "pick");
            Outcome::judge(&answer, |text| {
                text.contains("helper.sh") && text.contains("main.sh")
            })
        },
    },
    Probe {
        name: "bare name, warm",
        needs: "workspace/symbol after a file is open",
        run: |state| {
            let answer = show(state, Some("choose"), None, None);
            Outcome::judge(&answer, |text| {
                text.contains("helper.sh") && text.contains("choose()")
            })
        },
    },
];
