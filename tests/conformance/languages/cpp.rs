//! C++: clangd and ccls, and the one fixture here whose language splits a
//! symbol into a declaration and a definition.
//!
//! `helper.hpp` declares `Config::doubled` and `helper.cpp` defines it, so
//! two files name the same entity and neither is redundant. That is a shape
//! no other fixture has, and what a server does with it — one answer, two,
//! or an ambiguous listing — is the point of asking.
//!
//! `compile_flags.txt` is in the fixture because clangd without a
//! compilation database indexes only what it opens. Its absence would
//! measure the missing file rather than the server.

use crate::probe::{Candidate, Language, Probe, rename_preview, show, uses};
use crate::report::Outcome;

pub const CPP: Language = Language {
    name: "cpp",
    fixture: "cpp",
    extensions: &[],
    extra_files: &[],
    candidates: CANDIDATES,
    probes: PROBES,
};

const CANDIDATES: &[Candidate] = &[
    Candidate {
        name: "clangd",
        argv: &["clangd"],
        version_argv: &["clangd", "--version"],
        builtin: false,
        settings: "",
    },
    Candidate {
        name: "ccls",
        argv: &["ccls"],
        version_argv: &["ccls", "--version"],
        builtin: false,
        settings: "",
    },
];

const PROBES: &[Probe] = &[
    Probe {
        name: "bare name",
        needs: "workspace/symbol",
        run: |state| {
            let answer = show(state, Some("choose"), None, None);
            Outcome::judge(&answer, |text| {
                text.contains("helper.cpp") && text.contains("int choose(bool flag)")
            })
        },
    },
    Probe {
        name: "file-scoped name",
        needs: "documentSymbol",
        run: |state| {
            let answer = show(state, Some("identity"), Some("helper.cpp"), None);
            Outcome::judge(&answer, |text| text.contains("int identity(int value)"))
        },
    },
    Probe {
        name: "qualified name",
        needs: "outline containers",
        run: |state| {
            let answer = show(state, Some("Config::doubled"), None, None);
            Outcome::judge(&answer, |text| text.contains("doubled"))
        },
    },
    Probe {
        name: "qualified name, in file",
        needs: "outline nesting",
        run: |state| {
            let answer = show(state, Some("Config::doubled"), Some("helper.hpp"), None);
            Outcome::judge(&answer, |text| text.contains("doubled"))
        },
    },
    Probe {
        name: "cross-file uses",
        needs: "references",
        run: |state| {
            let answer = uses(state, "choose");
            Outcome::judge(&answer, |text| text.contains("main.cpp"))
        },
    },
    Probe {
        name: "anchor to a local",
        needs: "definition at a position",
        run: |state| {
            let answer = show(
                state,
                None,
                Some("main.cpp"),
                Some(("int count = config.doubled();", "doubled")),
            );
            Outcome::judge(&answer, |text| text.contains("doubled"))
        },
    },
    Probe {
        name: "rename preview",
        needs: "rename WorkspaceEdit",
        run: |state| {
            let answer = rename_preview(state, "choose", "pick");
            Outcome::judge(&answer, |text| {
                text.contains("helper.cpp") && text.contains("main.cpp")
            })
        },
    },
    Probe {
        name: "bare name, warm",
        needs: "workspace/symbol after a file is open",
        run: |state| {
            let answer = show(state, Some("choose"), None, None);
            Outcome::judge(&answer, |text| {
                text.contains("helper.cpp") && text.contains("int choose(bool flag)")
            })
        },
    },
];
