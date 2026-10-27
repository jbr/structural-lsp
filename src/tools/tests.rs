//! What a model reads about the tools: the descriptions `tools/list`
//! advertises, generated from the tools' `///` comments.

use super::Tools;
use mcplease::traits::AsToolsList;
use serde_json::Value;

/// The top-level directories of this repository whose paths a citation
/// would name.
const RECORDS: [&str; 5] = ["docs/", "backlog/", "journal/", "process/", "src/"];

/// Every repository path in `text`: one of [`RECORDS`] at the start of a
/// word, followed by a path character.
fn repository_paths(text: &str) -> Vec<String> {
    let mut found = Vec::new();
    for record in RECORDS {
        for (at, _) in text.match_indices(record) {
            let starts_word = text[..at]
                .chars()
                .next_back()
                .is_none_or(|before| !(before.is_alphanumeric() || before == '_'));
            let rest = &text[at + record.len()..];
            let path: String = rest
                .chars()
                .take_while(|c| c.is_alphanumeric() || matches!(c, '_' | '.' | '/' | '-'))
                .collect();
            if starts_word && !path.is_empty() {
                found.push(format!("{record}{path}"));
            }
        }
    }
    found
}

/// Examples are callers' arguments — `within: "src/agent"` is a directory a
/// caller names, not a citation — so they come out before the check.
fn without_examples(value: &mut Value) {
    match value {
        Value::Object(map) => {
            map.remove("examples");
            map.values_mut().for_each(without_examples);
        }
        Value::Array(items) => items.iter_mut().for_each(without_examples),
        _ => {}
    }
}

#[test]
fn tool_descriptions_name_no_repository_path() {
    // An embedder advertises these descriptions under its own name, and a
    // model reading them has no repository to look in.
    let mut tools = serde_json::to_value(<Tools as AsToolsList>::tools_list()).unwrap();
    without_examples(&mut tools);
    let cited = repository_paths(&tools.to_string());
    assert!(cited.is_empty(), "tool descriptions name {cited:?}");
}

#[test]
fn the_check_finds_a_citation_and_spares_a_word_ending_in_one() {
    assert_eq!(
        repository_paths("listed rather than picked (docs/resolution.md)."),
        ["docs/resolution.md"]
    );
    assert!(repository_paths("autodocs/ and mysrc/x are not citations").is_empty());
    assert!(repository_paths("a bare docs/ names no file").is_empty());
}
