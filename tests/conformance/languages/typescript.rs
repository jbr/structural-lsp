//! TypeScript: three candidates that disagree about what a project is.
//!
//! `typescript-language-server` wraps tsserver and reads `tsconfig.json`;
//! `deno lsp` has its own module resolution and ignores it. Both are
//! recorded by the dataset for `.ts` — typescript-language-server by twelve
//! sources, the most of any server in it — so this is the first language
//! here where two servers are genuinely alternatives. `vtsls` is a third
//! such wrapper, added for the measured order.

use crate::probe::{Candidate, Language, Probe, rename_preview, show, uses};
use crate::report::Outcome;

pub const TYPESCRIPT: Language = Language {
    name: "typescript",
    fixture: "typescript",
    extensions: &[],
    extra_files: &[],
    candidates: CANDIDATES,
    probes: PROBES,
};

const CANDIDATES: &[Candidate] = &[
    Candidate {
        name: "typescript-language-server",
        argv: &["typescript-language-server", "--stdio"],
        version_argv: &["typescript-language-server", "--version"],
        builtin: false,
        settings: "",
        extra_config: "",
    },
    Candidate {
        name: "deno",
        argv: &["deno", "lsp"],
        version_argv: &["deno", "--version"],
        builtin: false,
        // Measured: deno 2.9.6 serves **nothing** for the whole session
        // without this, and ignores the same value in the initialization
        // options — `workspace/configuration` is the only channel it reads
        // (`../../../docs/servers.md`). `unstable` and the inlay-hint
        // settings an editor also sets make no difference to these probes.
        settings: "{ enable = true }",
        extra_config: "",
    },
    Candidate {
        name: "vtsls",
        argv: &["vtsls", "--stdio"],
        version_argv: &["vtsls", "--version"],
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
                text.contains("helper.ts") && text.contains("function choose")
            })
        },
    },
    Probe {
        name: "file-scoped name",
        needs: "documentSymbol",
        run: |state| {
            let answer = show(state, Some("identity"), Some("helper.ts"), None);
            Outcome::judge(&answer, |text| text.contains("function identity"))
        },
    },
    Probe {
        name: "qualified name",
        needs: "outline containers",
        run: |state| {
            let answer = show(state, Some("Config.doubled"), None, None);
            Outcome::judge(&answer, |text| text.contains("doubled(): number"))
        },
    },
    Probe {
        name: "qualified name, in file",
        needs: "outline nesting",
        run: |state| {
            let answer = show(state, Some("Config.doubled"), Some("helper.ts"), None);
            Outcome::judge(&answer, |text| text.contains("doubled(): number"))
        },
    },
    Probe {
        name: "cross-file uses",
        needs: "references",
        run: |state| {
            let answer = uses(state, "choose");
            Outcome::judge(&answer, |text| text.contains("main.ts"))
        },
    },
    Probe {
        name: "anchor to a local",
        needs: "definition at a position",
        run: |state| {
            let answer = show(
                state,
                None,
                Some("main.ts"),
                Some(("let count = config.doubled();", "doubled")),
            );
            Outcome::judge(&answer, |text| text.contains("doubled(): number"))
        },
    },
    Probe {
        name: "rename preview",
        needs: "rename WorkspaceEdit",
        run: |state| {
            let answer = rename_preview(state, "choose", "pick");
            Outcome::judge(&answer, |text| {
                text.contains("helper.ts") && text.contains("main.ts")
            })
        },
    },
    Probe {
        name: "bare name, warm",
        needs: "workspace/symbol after a file is open",
        run: |state| {
            let answer = show(state, Some("choose"), None, None);
            Outcome::judge(&answer, |text| {
                text.contains("helper.ts") && text.contains("function choose")
            })
        },
    },
];
