use super::*;

#[test]
fn parses_both_annotation_kinds() {
    let text = "// @decl Config | Config\npub struct Config;\n// @expr limit | self.limit * 2\nx\n";
    let annotations = parse_annotations(text);
    assert_eq!(annotations.len(), 2);
    assert_eq!(annotations[0].kind, AnnKind::Decl);
    assert_eq!(annotations[0].query, "Config");
    assert_eq!(annotations[0].selection, "Config");
    assert_eq!(annotations[1].kind, AnnKind::Expr);
    assert_eq!(annotations[1].query, "limit");
    assert_eq!(annotations[1].selection, "self.limit * 2");
}

#[test]
fn resolves_selection_and_query_token_after_the_annotation() {
    let text = "// @expr doubled | config.doubled()\nlet x = config.doubled();\n";
    let annotations = parse_annotations(text);
    let resolved = resolve_annotation(text, &annotations[0]).unwrap();
    assert_eq!(
        &text[resolved.selection_start..resolved.selection_end],
        "config.doubled()"
    );
    assert_eq!(
        &text[resolved.target_offset..resolved.target_offset + 7],
        "doubled"
    );
    assert_eq!(resolved.target_provenance, "query-in-selection");
}

#[test]
fn query_absent_from_selection_falls_back_to_selection_start_with_provenance() {
    // `beyond` does not occur in the selection but does occur on a later
    // line. The target must be the selection start, with provenance saying
    // the query was not actually located — never the later occurrence.
    let text = "// @expr beyond | alpha()\nlet x = alpha();\nlet beyond = 1;\n";
    let annotations = parse_annotations(text);
    let resolved = resolve_annotation(text, &annotations[0]).unwrap();
    assert_eq!(
        &text[resolved.selection_start..resolved.selection_end],
        "alpha()"
    );
    assert_eq!(resolved.target_offset, resolved.selection_start);
    assert_eq!(resolved.target_provenance, "selection-start");
}

#[test]
fn selection_missing_from_the_annotations_region_is_unresolved() {
    // The first annotation's selection (`alpha`) only appears after the
    // second annotation begins, so it must not match that later text.
    let text = "// @decl first | alpha\n// @decl second | second\nsecond\n";
    let annotations = parse_annotations(text);
    assert!(
        resolve_annotation(text, &annotations[0]).is_none(),
        "a selection past its own region must not resolve against later source"
    );
    // The second annotation's selection is in its own region, so it does.
    let resolved = resolve_annotation(text, &annotations[1]).unwrap();
    assert_eq!(
        &text[resolved.selection_start..resolved.selection_end],
        "second"
    );
}

#[test]
fn declaration_selection_resolves_within_its_own_region() {
    let text = "// @decl smoke | smoke\n#[test]\nfn smoke() {}\n";
    let annotations = parse_annotations(text);
    let resolved = resolve_annotation(text, &annotations[0]).unwrap();
    assert_eq!(
        &text[resolved.selection_start..resolved.selection_end],
        "smoke"
    );
    assert_eq!(resolved.target_offset, resolved.selection_start);
    assert_eq!(resolved.target_provenance, "query-in-selection");
}

#[test]
fn positions_count_utf16_units() {
    let text = "a\n\u{1F600}b";
    // The emoji is one scalar but two UTF-16 code units.
    assert_eq!(
        position(text, 0),
        Position {
            line: 0,
            character: 0
        }
    );
    assert_eq!(
        position(text, 2),
        Position {
            line: 1,
            character: 0
        }
    );
    assert_eq!(
        position(text, 6),
        Position {
            line: 1,
            character: 2
        }
    );
}

#[test]
fn nonempty_treats_empty_collections_as_empty() {
    assert!(!is_nonempty(&Value::Null));
    assert!(!is_nonempty(&json!([])));
    assert!(!is_nonempty(&json!({})));
    assert!(is_nonempty(&json!([1])));
    assert!(is_nonempty(&json!(false))); // a real boolean answer is information
}
