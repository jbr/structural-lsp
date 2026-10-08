//! The order among candidates, asserted against the **stated** rule rather
//! than against a second run: score descending, then consensus
//! descending, then name ascending.

use super::*;

fn measurement() -> Measurement {
    Measurement {
        server_version: "1.0.0".to_string(),
        structural_revision: "abc1234".to_string(),
        fixture_revision: "fixture-1".to_string(),
        date: "2026-09-18".to_string(),
    }
}

fn entry(name: &str, language: &str, score: Option<u8>, consensus: u16) -> Entry {
    Entry {
        name: name.to_string(),
        command: vec![name.to_string()],
        serves: vec![Serves::known(language).expect("test language is known")],
        score: score.map(|answered| Score {
            answered,
            probes: 7,
            measurement: measurement(),
        }),
        consensus,
        provenance: Provenance {
            source: "test".to_string(),
            extensions: Confidence::Exact,
        },
    }
}

fn names(entries: &[&Entry]) -> Vec<String> {
    entries.iter().map(|e| e.name.clone()).collect()
}

#[test]
fn score_orders_before_anything_else() {
    // The whole premise: what a server answered through our own tools
    // beats how many editors ship it. `popular` has every editor and
    // answers less.
    let catalogue = Catalogue::new(vec![
        entry("popular", "python", Some(3), 12),
        entry("capable", "python", Some(6), 1),
    ]);

    assert_eq!(names(&catalogue.ranked()), vec!["capable", "popular"]);
}

#[test]
fn consensus_breaks_a_tie_in_score() {
    // Among servers that answered equally well, editor agreement stands
    // in for maintained-and-preferred. This is the only job it has.
    let catalogue = Catalogue::new(vec![
        entry("obscure", "python", Some(5), 1),
        entry("widespread", "python", Some(5), 9),
    ]);

    assert_eq!(names(&catalogue.ranked()), vec!["widespread", "obscure"]);
}

#[test]
fn name_is_the_terminal_rule_so_the_order_is_total() {
    // Without this the order depends on which entry the catalogue
    // happened to list first, and a plan could not be reproduced from a
    // report.
    let catalogue = Catalogue::new(vec![
        entry("zeta", "python", Some(5), 3),
        entry("alpha", "python", Some(5), 3),
    ]);

    assert_eq!(names(&catalogue.ranked()), vec!["alpha", "zeta"]);
}

#[test]
fn an_unmeasured_server_ranks_at_the_floor() {
    // Unmeasured is not the same as measured badly, and it is not treated
    // as better either: the premise is preferring what we verified, so
    // the way to promote a server is to measure it.
    let catalogue = Catalogue::new(vec![
        entry("unmeasured", "python", None, 99),
        entry("barely", "python", Some(1), 0),
    ]);

    assert_eq!(names(&catalogue.ranked()), vec!["barely", "unmeasured"]);
}

#[test]
fn a_zero_score_is_still_in_the_catalogue() {
    // There is no admission threshold: a bar would fire on the cases
    // `answering-is-availability.md` examined and rejected, and removing
    // the only candidate for a language does not improve either of them.
    let catalogue = Catalogue::new(vec![entry("serves-nothing", "python", Some(0), 1)]);

    assert_eq!(names(&catalogue.ranked()), vec!["serves-nothing"]);
    assert_eq!(
        names(&catalogue.candidates_for("python")),
        vec!["serves-nothing"]
    );
}

#[test]
fn a_score_cannot_be_stated_without_the_run_that_produced_it() {
    // Not a behaviour test but a type test, and the reason it is here: a
    // score is a joint measurement of server and client at a revision, so
    // one that cannot say what produced it is an assertion. If `Score`
    // ever gains a constructor that omits `Measurement`, this stops
    // compiling.
    let scored = entry("measured", "rust", Some(7), 0);
    let score = scored.score.expect("constructed with a score");
    assert_eq!(score.measurement.structural_revision, "abc1234");
    assert_eq!(score.answered, 7);
    assert_eq!(score.probes, 7);
}

#[test]
fn the_shipped_catalogue_claims_no_scores() {
    // The measured order is `docs/catalogue.md`'s; the generator that
    // emits it is `backlog/discovery/selection.md`'s A2. A number invented
    // here would be the unverified import the plan refuses, so this fails
    // the moment one appears without that work.

    for entry in Catalogue::built_in().ranked() {
        assert!(
            entry.score.is_none(),
            "{} carries a score with no synthesis behind it",
            entry.name
        );
    }
}

#[test]
fn every_catalogue_language_has_extensions() {
    for (language, extensions) in LANGUAGES {
        assert!(!extensions.is_empty(), "{language} identifies no files");
        assert_eq!(extensions_for_language(language), Some(*extensions));
    }
    assert_eq!(extensions_for_language("cobol"), None);
}
