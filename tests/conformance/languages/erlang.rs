//! Erlang: `elp` and `erlang_ls`, and the fixture whose language has no
//! type to qualify a name with.
//!
//! Every other column here can ask for a member of a type
//! (`Config.doubled`, `Config::doubled`). Erlang has no types carrying
//! functions: the container of a function is its **module**, and a
//! qualified call is written `module:function` — `helper:choose(true)` in
//! `src/main.erl`. So the qualified-name probes ask the nearest meaningful
//! question, "the `choose` in module `helper`", rather than one about a
//! type.
//!
//! Two things constrain how that question can be spelled.
//!
//! structural splits a qualified `symbol` on `::`, `.` and `/`, and on
//! nothing else (`src/tools/resolution/qualify.rs`). `:` is not a
//! separator, so `helper:choose` parses as a single bare name containing a
//! colon, which no index entry can match exactly. The probes therefore ask
//! `helper::choose`: the only spelling that reaches the qualifier filter at
//! all, and one whose qualifier — a module rather than a type — is the shape
//! `backlog/module-path-qualifiers.md` records as unable to resolve when the
//! server reports no `containerName`. Whether erlang_ls reports the module
//! as the container is what these two probes measure; a `no` here is the
//! finding, not a harness fault.
//!
//! The same splitting rules out asking by arity. Erlang names a function
//! `choose/1`, and `/` is a separator, so `choose/1` parses as the name `1`
//! qualified by `choose`. Every probe here asks the bare `choose`. If
//! erlang_ls reports document and workspace symbols with the arity in the
//! name, exact-name matching finds nothing and the whole column scores low
//! for that one reason.
//!
//! The tree is a conventional rebar3 application — `rebar.config`, an
//! `.app.src`, sources under `src/` — because erlang_ls locates code
//! through app directories rather than by walking the root. `erlang_ls.config`
//! names the root as the one app directory.

use crate::probe::{Candidate, Language, Probe, rename_preview, show, uses};
use crate::report::Outcome;

pub const ERLANG: Language = Language {
    name: "erlang",
    fixture: "erlang",
    // `config.rs` knows extensions for eleven language ids and "erlang" is
    // not among them, so they are spelled out here.
    extensions: &["erl", "hrl"],
    extra_files: &[],
    generated_files: &[],
    candidates: CANDIDATES,
    probes: PROBES,
};

const CANDIDATES: &[Candidate] = &[
    Candidate {
        name: "elp",
        argv: &["elp", "server"],
        version_argv: &["elp", "--version"],
        builtin: false,
        settings: "",
        extra_config: "",
    },
    Candidate {
        name: "erlang_ls",
        argv: &["erlang_ls", "--transport", "stdio"],
        version_argv: &["erlang_ls", "--version"],
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
                text.contains("helper.erl") && text.contains("choose(Flag)")
            })
        },
    },
    Probe {
        name: "file-scoped name",
        needs: "documentSymbol",
        run: |state| {
            let answer = show(state, Some("identity"), Some("src/helper.erl"), None);
            Outcome::judge(&answer, |text| text.contains("identity(Value)"))
        },
    },
    Probe {
        name: "qualified name",
        needs: "outline containers",
        run: |state| {
            // The container is the module `helper`, spelled with `::`
            // because `:` is not a separator structural splits on.
            let answer = show(state, Some("helper::choose"), None, None);
            Outcome::judge(&answer, |text| text.contains("choose(Flag)"))
        },
    },
    Probe {
        name: "qualified name, in file",
        needs: "outline nesting",
        run: |state| {
            let answer = show(state, Some("helper::choose"), Some("src/helper.erl"), None);
            Outcome::judge(&answer, |text| text.contains("choose(Flag)"))
        },
    },
    Probe {
        name: "cross-file uses",
        needs: "references",
        run: |state| {
            // `choose` is declared in src/helper.erl and called from
            // src/main.erl.
            let answer = uses(state, "choose");
            Outcome::judge(&answer, |text| text.contains("main.erl"))
        },
    },
    Probe {
        name: "anchor to a local",
        needs: "definition at a position",
        run: |state| {
            let answer = show(
                state,
                None,
                Some("src/main.erl"),
                Some(("Offset = helper:choose(true),", "choose")),
            );
            Outcome::judge(&answer, |text| text.contains("choose(Flag)"))
        },
    },
    Probe {
        name: "rename preview",
        needs: "rename WorkspaceEdit",
        run: |state| {
            let answer = rename_preview(state, "choose", "pick");
            Outcome::judge(&answer, |text| {
                text.contains("helper.erl") && text.contains("main.erl")
            })
        },
    },
    Probe {
        name: "bare name, warm",
        needs: "workspace/symbol after a file is open",
        run: |state| {
            let answer = show(state, Some("choose"), None, None);
            Outcome::judge(&answer, |text| {
                text.contains("helper.erl") && text.contains("choose(Flag)")
            })
        },
    },
];
