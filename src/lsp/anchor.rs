//! Anchors: quoted source text that locates a selection in a file, as a
//! pure function over the text read from disk.
//!
//! An anchor must occur exactly once in the file; a focus, when given, is a
//! substring of the anchor that must occur exactly once within it, counting
//! occurrences on identifier boundaries first and as plain substrings only
//! when none sit on one (docs/resolution.md). The result is the focus's byte
//! range, which the caller converts to a position with the server's
//! encoding. Nothing here guesses: zero or several occurrences are errors
//! that say where the occurrences are.

use anyhow::{Result, bail};

/// Where a focus sits in the file, as byte offsets, plus the 1-based line
/// of the anchor for messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub start: usize,
    pub end: usize,
    pub line: u32,
}

/// Locate `anchor` in `text`, then `focus` within it (the whole anchor
/// when `None`).
pub fn locate(text: &str, anchor: &str, focus: Option<&str>) -> Result<Selection> {
    if anchor.is_empty() {
        bail!("`anchor` must be non-empty source text");
    }
    let occurrences: Vec<usize> = text.match_indices(anchor).map(|(at, _)| at).collect();
    let anchor_start = match occurrences.as_slice() {
        [] => bail!("anchor not found in the file: `{}`", elide(anchor)),
        [one] => *one,
        many => {
            let lines: Vec<String> = many
                .iter()
                .take(8)
                .map(|&at| line_of(text, at).to_string())
                .collect();
            bail!(
                "anchor occurs {} times, at lines {}{}; quote more context so it occurs once",
                many.len(),
                lines.join(", "),
                if many.len() > 8 { ", …" } else { "" }
            )
        }
    };
    let (start, end) = match focus {
        None => (anchor_start, anchor_start + anchor.len()),
        Some(focus) => {
            if focus.is_empty() {
                bail!("`focus` must be non-empty text within the anchor");
            }
            let within = focus_occurrences(anchor, focus);
            match within.as_slice() {
                [] => bail!(
                    "focus `{}` does not occur in the anchor `{}`",
                    elide(focus),
                    elide(anchor)
                ),
                [one] => (anchor_start + one, anchor_start + one + focus.len()),
                many => bail!(
                    "focus `{}` occurs {} times in the anchor; quote a longer focus or a \
                     shorter anchor",
                    elide(focus),
                    many.len()
                ),
            }
        }
    };
    Ok(Selection {
        start,
        end,
        line: line_of(text, anchor_start),
    })
}

/// Byte offsets of `focus` within `anchor`: the occurrences on identifier
/// boundaries when there are any, otherwise every substring occurrence.
///
/// An occurrence is on a boundary when each edge of the focus that is an
/// identifier character is not continued by one in the anchor, so `boundaries`
/// does not match inside `extract_boundaries`, while a focus that starts or
/// ends in punctuation (`::new`, `$el`) is not constrained on that side. The
/// substring fallback keeps a focus that sits only inside longer words (a
/// fragment of a string literal or a path) resolvable as before.
fn focus_occurrences(anchor: &str, focus: &str) -> Vec<usize> {
    let substrings: Vec<usize> = anchor.match_indices(focus).map(|(at, _)| at).collect();
    let first = focus.chars().next().is_some_and(is_identifier_char);
    let last = focus.chars().next_back().is_some_and(is_identifier_char);
    let bounded: Vec<usize> = substrings
        .iter()
        .copied()
        .filter(|&at| {
            let before = anchor[..at].chars().next_back();
            let after = anchor[at + focus.len()..].chars().next();
            !(first && before.is_some_and(is_identifier_char))
                && !(last && after.is_some_and(is_identifier_char))
        })
        .collect();
    if bounded.is_empty() {
        substrings
    } else {
        bounded
    }
}

/// A character that continues an identifier in the languages served here:
/// Unicode alphanumerics and `_`. `$` and `-` are not included — they are
/// identifier characters in JavaScript, PHP, Lisp and CSS but sigils or
/// operators elsewhere — so a focus next to either still matches on a
/// boundary, and in `foo-bar` the focus `foo` is not shielded from the dash.
fn is_identifier_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// 1-based line of a byte offset.
fn line_of(text: &str, offset: usize) -> u32 {
    text[..offset].matches('\n').count() as u32 + 1
}

/// A quote short enough for an error line.
fn elide(text: &str) -> String {
    const MAX: usize = 60;
    if text.chars().count() <= MAX {
        text.to_string()
    } else {
        let mut cut: String = text.chars().take(MAX).collect();
        cut.push('…');
        cut
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEXT: &str =
        "fn run() {\n    let config = Config::new(3);\n    let count = config.doubled();\n}\n";

    #[test]
    fn a_unique_anchor_selects_itself_or_its_focus() {
        let whole = locate(TEXT, "let config = Config::new(3);", None).unwrap();
        assert_eq!(
            &TEXT[whole.start..whole.end],
            "let config = Config::new(3);"
        );
        assert_eq!(whole.line, 2);
        let focus = locate(TEXT, "let config = Config::new(3);", Some("new")).unwrap();
        assert_eq!(&TEXT[focus.start..focus.end], "new");
        assert_eq!(focus.line, 2);
    }

    #[test]
    fn zero_or_several_occurrences_are_stated_with_lines() {
        let missing = locate(TEXT, "nothing like this", None).unwrap_err();
        assert!(missing.to_string().starts_with("anchor not found"));
        let several = locate(TEXT, "config", None).unwrap_err().to_string();
        assert!(
            several.starts_with("anchor occurs 2 times, at lines 2, 3;"),
            "{several}"
        );
        let ambiguous_focus = locate(TEXT, "config.doubled()", Some("d"))
            .unwrap_err()
            .to_string();
        assert!(
            ambiguous_focus.contains("occurs 2 times in the anchor"),
            "{ambiguous_focus}"
        );
        let absent_focus = locate(TEXT, "config.doubled()", Some("limit"))
            .unwrap_err()
            .to_string();
        assert!(
            absent_focus.starts_with("focus `limit` does not occur"),
            "{absent_focus}"
        );
        assert!(locate(TEXT, "", None).is_err());
        assert!(locate(TEXT, "fn run", Some("")).is_err());
    }

    fn focused<'a>(text: &'a str, focus: &str) -> (&'a str, usize) {
        let found = locate(text, text, Some(focus)).unwrap();
        (&text[found.start..found.end], found.start)
    }

    #[test]
    fn a_focus_that_prefixes_or_suffixes_a_neighbour_resolves_on_its_boundary() {
        let line = "let (boundaries, elided) = extract_boundaries(log);";
        assert_eq!(focused(line, "boundaries"), ("boundaries", 5));
        // A suffix of a neighbour, and a prefix of one.
        let mixed = "renew(x) + new(y) + new_len(z)";
        assert_eq!(focused(mixed, "new_len"), ("new_len", 20));
        assert_eq!(focused("renew(x) + new(y)", "new"), ("new", 11));
        assert_eq!(focused("config_path(config)", "config"), ("config", 12));
        // Punctuation at an edge constrains nothing on that side.
        let path = "Config::new(3) + Config::news(4)";
        assert_eq!(focused(path, "::new"), ("::new", 6));
        assert_eq!(focused("$el + $element", "$el"), ("$el", 0));
        assert_eq!(focused("$el + $element", "el"), ("el", 1));
    }

    #[test]
    fn several_whole_identifier_occurrences_are_still_refused() {
        let line = "boundaries + boundaries + extract_boundaries()";
        let err = locate(line, line, Some("boundaries"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("occurs 2 times in the anchor"), "{err}");
        // Dashes do not join identifiers.
        let dashed = "(foo-bar foo)";
        assert!(locate(dashed, dashed, Some("foo")).is_err());
    }

    #[test]
    fn a_focus_with_no_boundary_occurrence_falls_back_to_substrings() {
        assert_eq!(focused("include!(\"xlibx.rs\")", "lib"), ("lib", 11));
        assert_eq!(focused("renew(x)", "new"), ("new", 2));
        let two = "renew(renewal)";
        let err = locate(two, two, Some("new")).unwrap_err().to_string();
        assert!(err.contains("occurs 2 times in the anchor"), "{err}");
    }

    #[test]
    fn boundaries_follow_unicode_alphanumerics() {
        let line = "let (größe, größer) = größen(größe2);";
        assert_eq!(focused(line, "größe"), ("größe", 5));
        assert_eq!(focused("let 名前 = 名前付き();", "名前"), ("名前", 4));
        assert_eq!(focused("é + é_x + éa", "é"), ("é", 0));
        // No whole-identifier occurrence: the substring fallback counts the
        // inner ones, and there are two.
        let inner = "größen(größer)";
        assert!(locate(inner, inner, Some("größe")).is_err());
    }
}
