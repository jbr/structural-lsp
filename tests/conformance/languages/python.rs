//! Python: the first language the probe was pointed at, and the one whose
//! six installed servers found four defects in `src/`
//! (`../../../journal/2026-09-17-conformance-probe-findings.md`).

use crate::probe::{Candidate, Language, Probe, rename_preview, show, uses};
use crate::report::Outcome;

pub const PYTHON: Language = Language {
    name: "python",
    fixture: "python",
    extensions: &[],
    extra_files: &[],
    candidates: CANDIDATES,
    probes: PROBES,
};

/// The Python candidates that are installed often enough to be worth the
/// harness. `ruff server` is deliberately absent: lsp-mode marks it an
/// add-on, and it serves diagnostics and formatting rather than the
/// navigation `show`/`uses`/`action` are built on.
const CANDIDATES: &[Candidate] = &[
    Candidate {
        name: "pyright",
        argv: &["pyright-langserver", "--stdio"],
        version_argv: &["pyright", "--version"],
        builtin: true,
        settings: "",
        extra_config: "",
    },
    Candidate {
        name: "basedpyright",
        argv: &["basedpyright-langserver", "--stdio"],
        version_argv: &["basedpyright", "--version"],
        builtin: false,
        settings: "",
        extra_config: "",
    },
    Candidate {
        name: "pylsp",
        argv: &["pylsp"],
        version_argv: &["pylsp", "--version"],
        builtin: false,
        settings: "",
        extra_config: "",
    },
    // Not installed here, but the argv is what the sources record, so the
    // probe picks them up on a machine that has them.
    Candidate {
        name: "jedi",
        argv: &["jedi-language-server"],
        version_argv: &["jedi-language-server", "--version"],
        builtin: false,
        settings: "",
        extra_config: "",
    },
    // ty's errors are structural's, not ty's: it reports the whole
    // declaration as a symbol's range, which the spec allows and
    // `verify_name_at` refuses (backlog/symbol-range-convention.md).
    Candidate {
        name: "ty",
        argv: &["ty", "server"],
        version_argv: &["ty", "--version"],
        builtin: false,
        settings: "",
        extra_config: "",
    },
    Candidate {
        name: "pyrefly",
        argv: &["pyrefly", "lsp"],
        version_argv: &["pyrefly", "--version"],
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
                text.contains("helper.py") && text.contains("def choose")
            })
        },
    },
    Probe {
        name: "file-scoped name",
        needs: "documentSymbol",
        run: |state| {
            let answer = show(state, Some("identity"), Some("helper.py"), None);
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
        // of the index. `workspace/symbol` carries one optional
        // `containerName`, and several servers leave it empty or name the
        // module; `documentSymbol` nests, so the class is structural in the
        // reply rather than a field a server may omit. A `no` on the
        // previous column and a `yes` here says the container is missing
        // from the *index*, which is a catalogue fact about the server and
        // not a limit of qualified names.
        name: "qualified name, in file",
        needs: "outline nesting",
        run: |state| {
            let answer = show(state, Some("Config.doubled"), Some("helper.py"), None);
            Outcome::judge(&answer, |text| text.contains("def doubled"))
        },
    },
    Probe {
        name: "cross-file uses",
        needs: "references",
        run: |state| {
            let answer = uses(state, "choose");
            // The definition is in helper.py and the only call is in
            // main.py: a server that reports just the definition has not
            // searched the project.
            Outcome::judge(&answer, |text| text.contains("main.py"))
        },
    },
    Probe {
        name: "anchor to a local",
        needs: "definition at a position",
        run: |state| {
            let answer = show(
                state,
                None,
                Some("main.py"),
                Some(("count = config.doubled()", "doubled")),
            );
            Outcome::judge(&answer, |text| text.contains("def doubled"))
        },
    },
    Probe {
        name: "rename preview",
        needs: "rename WorkspaceEdit",
        run: |state| {
            let answer = rename_preview(state, "choose", "pick");
            // Two files, because the call site is in the other one. A
            // rename that rewrites only the definition would break the
            // caller's code.
            Outcome::judge(&answer, |text| {
                text.contains("helper.py") && text.contains("main.py")
            })
        },
    },
    Probe {
        name: "bare name, warm",
        needs: "workspace/symbol after a file is open",
        run: |state| {
            // The same query as the first probe, run last. The probes share
            // one session, so by here every fixture file has been opened by
            // some earlier query. A `no` on the cold probe and a `yes` here
            // says the server's symbol index is empty until a file is
            // opened — a different catalogue fact from "cannot serve
            // workspace/symbol", and the same shape as
            // `backlog/bare-name-staleness.md`.
            let answer = show(state, Some("choose"), None, None);
            Outcome::judge(&answer, |text| {
                text.contains("helper.py") && text.contains("def choose")
            })
        },
    },
];
