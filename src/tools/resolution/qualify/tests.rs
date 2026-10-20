use super::*;

#[test]
fn names_split_on_any_qualifier_separator() {
    let parsed = parse("Config::new").unwrap();
    assert_eq!(parsed.name, "new");
    assert_eq!(parsed.qualifiers, ["Config"]);
    assert_eq!(parse("Config.doubled").unwrap().qualifiers, ["Config"]);
    assert_eq!(parse("a::b.c/d").unwrap().qualifiers, ["a", "b", "c"]);
    assert_eq!(
        parse("  choose ").unwrap(),
        Qualified {
            name: "choose".into(),
            qualifiers: vec![]
        }
    );
    assert_eq!(parse("Config::new").unwrap().display(), "Config::new");
    assert!(parse("").is_err());
    assert!(parse("Config::").is_err());
    assert!(parse("::new").is_err());
}

/// gopls spells the receiver into the name: `Config.Doubled` from the
/// index, `(Config).Doubled` from an outline. Both name `Doubled` and
/// both say its container is `Config`.
#[test]
fn a_server_may_spell_the_container_into_the_name() {
    for advertised in ["Config.Doubled", "(Config).Doubled", "Config::Doubled"] {
        assert!(advertises(advertised, "Doubled"), "{advertised}");
        assert_eq!(
            advertised_container(advertised, "Doubled").as_deref(),
            Some("Config"),
            "{advertised}"
        );
    }
    // Several segments are kept in order, so a module path qualifies
    // the same way a type does.
    assert_eq!(
        advertised_container("pkg/inner.Doubled", "Doubled").as_deref(),
        Some("pkg::inner")
    );
}

/// The last segment must be the name asked for, and a name with no
/// separator is never reinterpreted.
#[test]
fn only_the_last_segment_resolves_and_a_plain_name_is_untouched() {
    assert!(advertises("Doubled", "Doubled"));
    assert_eq!(advertised_container("Doubled", "Doubled"), None);
    // The name is the container's, not the symbol's.
    assert!(!advertises("Config.Doubled", "Config"));
    // A different trailing segment is a different symbol.
    assert!(!advertises("Config.Other", "Doubled"));
    // Interior punctuation is left alone: only the ends are trimmed.
    assert_eq!(
        advertised_container("a-b.Doubled", "Doubled").as_deref(),
        Some("a-b")
    );
}

/// A candidate as the crate builds them — from JSON, because the struct
/// literal would name a deprecated field.
fn candidate(name: &str, container: Option<&str>, path: Option<&str>) -> Candidate {
    Candidate {
        server: "s".into(),
        info: serde_json::from_value(serde_json::json!({
            "name": name,
            "kind": 12,
            "location": {
                "uri": "file:///x",
                "range": {
                    "start": { "line": 0, "character": 0 },
                    "end": { "line": 0, "character": 0 }
                }
            },
            "containerName": container,
        }))
        .expect("a SymbolInformation"),
        canonical_path: path.map(std::path::PathBuf::from),
        exact: true,
    }
}

/// Split candidates under the view `/ws`, with no outline asked.
fn split(candidates: Vec<Candidate>, asked: &str) -> (Vec<Candidate>, Vec<Candidate>) {
    let placed = candidates
        .into_iter()
        .map(|candidate| Placed::new(Path::new("/ws"), candidate))
        .collect();
    partition(placed, &parse(asked).unwrap())
}

/// The files the kept candidates are in.
fn files(kept: &[Candidate]) -> Vec<&str> {
    kept.iter()
        .map(|candidate| {
            candidate
                .canonical_path
                .as_deref()
                .and_then(Path::to_str)
                .unwrap_or("")
        })
        .collect()
}

/// Either source confirming the qualifier is enough, because they are
/// alternative places to say the same thing: ty reports no container at
/// all, gopls reports the package in `containerName` and the type in
/// the name.
#[test]
fn either_container_source_satisfies_the_qualifier() {
    // The field says it (pyright's shape).
    let (kept, _) = split(
        vec![candidate("Doubled", Some("Config"), None)],
        "Config::Doubled",
    );
    assert_eq!(kept.len(), 1, "containerName confirms it");

    // The name says it and the field names the package (gopls's shape).
    let gopls = candidate("Config.Doubled", Some("example.com/scratch"), None);
    let (kept, _) = split(vec![gopls], "Config::Doubled");
    assert_eq!(kept.len(), 1, "the advertised name confirms it");

    // Neither says it: listed, not silently dropped.
    let (kept, unqualified) = split(
        vec![candidate("Doubled", Some("Other"), None)],
        "Config::Doubled",
    );
    assert!(kept.is_empty());
    assert_eq!(unqualified.len(), 1);
}

/// gleam and qmlls report no container at all: the module is the file,
/// and the file name is not in the reply.
#[test]
fn a_module_that_is_a_file_qualifies_from_the_path() {
    let choose = |path| candidate("choose", None, Some(path));
    let (kept, _) = split(vec![choose("/ws/src/helper.gleam")], "helper.choose");
    assert_eq!(kept.len(), 1, "the module is the file");

    let (kept, _) = split(
        vec![candidate("doubled", None, Some("/ws/Config.qml"))],
        "Config.doubled",
    );
    assert_eq!(kept.len(), 1, "the component is the file");

    // Another file's symbol is not in this module.
    let (kept, unqualified) = split(vec![choose("/ws/src/other.gleam")], "helper.choose");
    assert!(kept.is_empty());
    assert_eq!(unqualified.len(), 1);

    // A file outside the view still names its own module.
    let (kept, _) = split(vec![choose("/elsewhere/helper.gleam")], "helper.choose");
    assert_eq!(kept.len(), 1, "the stem alone");
}

/// rust-analyzer reports no container for a free function, so a module path
/// is confirmed from the file's place under the view: each directory, then
/// the stem, and a reported container after them.
#[test]
fn a_module_path_qualifies_from_the_directories_and_the_file() {
    let render = |path| candidate("render", None, Some(path));
    let both = || {
        vec![
            render("/ws/src/tools/listing.rs"),
            render("/ws/src/lsp/listing.rs"),
        ]
    };
    for asked in ["tools::listing::render", "src.tools.listing.render"] {
        let (kept, _) = split(both(), asked);
        assert_eq!(files(&kept), ["/ws/src/tools/listing.rs"], "{asked}");
    }
    // Nothing above the view is a segment, and a wrong directory is wrong.
    for asked in ["ws::src::tools::listing::render", "other::listing::render"] {
        let (kept, _) = split(both(), asked);
        assert!(kept.is_empty(), "{asked}: {:?}", files(&kept));
    }
    // Two files that end the same way are both kept: the listing, not a pick.
    let (kept, _) = split(both(), "listing::render");
    assert_eq!(kept.len(), 2);

    // A method: the file, then the type the server reported.
    let new = |path| candidate("new", Some("Config"), Some(path));
    let (kept, _) = split(
        vec![new("/ws/src/config.rs"), new("/ws/src/other.rs")],
        "config::Config::new",
    );
    assert_eq!(files(&kept), ["/ws/src/config.rs"]);
}

/// A `mod tests` inside a file is in no path and in no container the index
/// reports; the outline's nesting supplies it, after the file's path as well
/// as alone, and it is what the listing then shows.
#[test]
fn an_inline_module_qualifies_from_the_outline_nesting() {
    let helper = candidate("helper", None, Some("/ws/src/lsp/outline.rs"));
    let asked = parse("outline::tests::helper").unwrap();
    let mut place = Placed::new(Path::new("/ws"), helper);
    assert_eq!(place.closeness(&asked), None);
    place.set_nesting(Some("tests".into()));
    assert_eq!(place.closeness(&asked), Some(Closeness::Place));
    assert_eq!(
        place.closeness(&parse("tests::helper").unwrap()),
        Some(Closeness::Place)
    );
    // Still in the file, but no longer its top.
    assert_eq!(
        place.closeness(&parse("outline::helper").unwrap()),
        Some(Closeness::InFile)
    );
    assert_eq!(
        place.candidate.info.container_name.as_deref(),
        Some("tests")
    );
}

/// The function at the top of a file is what its module path names; a
/// same-named helper in the file's `mod tests` is named by `tests`.
#[test]
fn the_top_of_a_file_is_closer_than_what_it_encloses() {
    let file = "/ws/src/tools/listing.rs";
    let placed = |nesting: Option<&str>| {
        let mut place = Placed::new(Path::new("/ws"), candidate("render", None, Some(file)));
        place.set_nesting(nesting.map(str::to_string));
        place
    };
    let split = |asked: &str| {
        let (kept, _) = partition(
            vec![placed(None), placed(Some("tests"))],
            &parse(asked).unwrap(),
        );
        kept.iter()
            .map(|candidate| candidate.info.container_name.clone())
            .collect::<Vec<_>>()
    };
    assert_eq!(split("tools::listing::render"), [None]);
    assert_eq!(split("listing::render"), [None]);
    assert_eq!(split("listing::tests::render"), [Some("tests".to_string())]);
    // By directory, the same order holds.
    assert_eq!(split("tools::render"), [None]);
}

/// The outline is asked only when it could change the answer.
#[test]
fn the_outline_is_asked_only_when_it_could_change_the_answer() {
    let place = |container, path| Placed::new(Path::new("/ws"), candidate("new", container, path));
    let asked = parse("tests::Config::new").unwrap();
    let never = || panic!("the text is not needed");
    // No container, and the qualifier reaches nothing by path: only when the
    // text could hold what the outline would contribute.
    assert!(place(None, None).asks_outline(&asked, || true));
    assert!(!place(None, None).asks_outline(&asked, || false));
    // No container, and the qualifier reaches the file: whether the
    // candidate is at its top or inside something decides how close.
    let in_file = place(None, Some("/ws/src/tests.rs"));
    assert!(in_file.asks_outline(&parse("tests::new").unwrap(), never));
    // A reported container that ends the way the qualifier does, which is
    // longer: the outline's nesting could be the rest.
    assert!(place(Some("Config"), None).asks_outline(&asked, never));
    // Already a place, or a container that ends another way.
    assert!(!place(Some("Config"), None).asks_outline(&parse("Config::new").unwrap(), never));
    assert!(!place(Some("Other"), None).asks_outline(&asked, never));
    // No qualifier, nothing to ask.
    assert!(!place(None, None).asks_outline(&parse("new").unwrap(), never));
}

/// The directory is the module for `mod.rs` and `__init__.py`, but only when
/// nothing matched by file or container: a file of that name settles it.
#[test]
fn the_directory_is_the_module_only_when_nothing_else_matched() {
    let run = |path| candidate("run", None, Some(path));
    for (path, asked) in [
        ("/ws/src/host/observation/mod.rs", "observation::run"),
        ("/ws/src/host/observation/mod.rs", "host::observation::run"),
        ("/ws/pkg/__init__.py", "pkg.run"),
        ("/ws/src/tools/listing.rs", "tools::run"),
    ] {
        let (kept, _) = split(vec![run(path)], asked);
        assert_eq!(kept.len(), 1, "{asked}");
    }

    // `views.rs` is `views`; `views/calls.rs` is only in its directory.
    let (kept, _) = split(
        vec![
            run("/ws/src/views/calls.rs"),
            run("/ws/src/views.rs"),
            run("/ws/src/views/hover.rs"),
        ],
        "views::run",
    );
    assert_eq!(files(&kept), ["/ws/src/views.rs"]);

    // Without it, every match by directory is kept, for the listing.
    let (kept, _) = split(
        vec![run("/ws/src/views/calls.rs"), run("/ws/src/views/hover.rs")],
        "views::run",
    );
    assert_eq!(kept.len(), 2);

    // Only the directory the file is in, and a file directly under the
    // view has none.
    let (kept, _) = split(vec![run("/ws/src/views/calls.rs")], "src::run");
    assert!(kept.is_empty());
    let (kept, _) = split(vec![run("/ws/lib.rs")], "ws::run");
    assert!(kept.is_empty());
}

#[test]
fn a_qualifier_is_a_suffix_of_the_container() {
    let q = |parts: &[&str]| parts.iter().map(|p| p.to_string()).collect::<Vec<_>>();
    assert!(container_matches(Some("Config"), &q(&["Config"])));
    assert!(container_matches(
        Some("crate::config::Config"),
        &q(&["Config"])
    ));
    assert!(container_matches(
        Some("config::Config"),
        &q(&["config", "Config"])
    ));
    assert!(!container_matches(Some("Other"), &q(&["Config"])));
    assert!(!container_matches(None, &q(&["Config"])));
    assert!(container_matches(None, &q(&[])));
    assert!(!container_matches(Some("Config"), &q(&["mod", "Config"])));
}
