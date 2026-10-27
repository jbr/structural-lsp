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
use std::path::Path;

pub const CPP: Language = Language {
    name: "cpp",
    fixture: "cpp",
    extensions: &[],
    extra_files: &[],
    generated_files: &[("compile_commands.json", compile_commands)],
    candidates: CANDIDATES,
    probes: PROBES,
};

/// A compilation database for the scratch tree, rendered per run: the
/// entries name absolute paths, which a checked-in fixture cannot hold.
/// One entry per translation unit with the fixture's flags; headers
/// compile through the units that include them.
fn compile_commands(root: &Path) -> String {
    let entry = |file: &str| {
        format!(
            "{{\n    \"directory\": \"{root}\",\n    \"command\": \"clang++ -std=c++17 -I{root} -c {root}/{file}\",\n    \"file\": \"{root}/{file}\"\n}}",
            root = root.display(),
        )
    };
    format!("[\n{},\n{}\n]\n", entry("helper.cpp"), entry("main.cpp"))
}

const CANDIDATES: &[Candidate] = &[
    Candidate {
        name: "clangd",
        argv: &["clangd"],
        version_argv: &["clangd", "--version"],
        builtin: false,
        settings: "",
        extra_config: "",
    },
    Candidate {
        name: "ccls",
        argv: &["ccls"],
        version_argv: &["ccls", "--version"],
        builtin: false,
        settings: "",
        // ccls sends one `window/workDoneProgress/create` for an `index`
        // token at startup and no `$/progress` at all, whatever the size of
        // the tree. Counting that create leaves the connection never idle,
        // so every readiness wait on it blocks forever
        // (`../../journal/2026-09-19-ccls-reserves-a-token.md`).
        extra_config: "progress-create-is-work = false\n",
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
