//! Anchors: quoted source text that locates a selection in a file, as a
//! pure function over the text read from disk.
//!
//! An anchor must occur exactly once in the file; a focus, when given, is a
//! substring of the anchor that must occur exactly once within it. The
//! result is the focus's byte range, which the caller converts to a
//! position with the server's encoding. Nothing here guesses: zero or
//! several occurrences are errors that say where the occurrences are.

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
            let within: Vec<usize> = anchor.match_indices(focus).map(|(at, _)| at).collect();
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
}
