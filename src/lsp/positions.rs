//! LSP position conversions between byte offsets and UTF-16 code-unit
//! positions — honest in both directions, rejecting rather than clamping.
//!
//! LSP positions are UTF-16 code units (unless a server negotiated another
//! encoding; the sessions in this crate advertise `utf-16` only). On ASCII
//! input UTF-16 columns equal byte columns, which is why the spike got away
//! with a one-direction ASCII-only helper; this module is the general version
//! and every invalid input is an error, never a silent clamp or panic:
//!
//! - a byte offset that is not on a UTF-8 character boundary, or past the end
//!   of the text;
//! - a line past the end of the text;
//! - a column past the end of its line;
//! - a column that falls *inside* a surrogate pair (an odd UTF-16 boundary
//!   that would point between the two halves of an astral character).

use anyhow::{Result, bail};
use lsp_types::Position;

/// LSP line terminators, per the spec: `"\r\n"`, `"\n"`, or a bare `"\r"`.
/// Length of the terminator starting at `index`, if any.
fn terminator_len(text: &str, index: usize) -> Option<usize> {
    let bytes = text.as_bytes();
    match bytes.get(index) {
        Some(b'\r') => Some(if bytes.get(index + 1) == Some(&b'\n') {
            2
        } else {
            1
        }),
        Some(b'\n') => Some(1),
        _ => None,
    }
}

/// Convert a byte offset to an LSP position.
///
/// Line terminators are not line content: an offset pointing at the
/// terminator is the end of that line, and an offset pointing *inside* one
/// (between the `\r` and the `\n` of a CRLF) is rejected rather than silently
/// resolved to either side.
/// Convert a byte offset to an LSP position. The reverse half of
/// [`position_to_offset`]; the tool path only needs the reverse conversion —
/// this direction is exercised by the round-trip tests.
#[allow(dead_code)]
pub fn offset_to_position(text: &str, offset: usize) -> Result<Position> {
    if offset > text.len() {
        bail!(
            "byte offset {offset} is past the end of {len}-byte text",
            len = text.len()
        );
    }
    if !text.is_char_boundary(offset) {
        bail!("byte offset {offset} is not on a character boundary");
    }
    let mut line = 0u32;
    let mut character = 0u32;
    let mut index = 0usize;
    while index < offset {
        if let Some(len) = terminator_len(text, index) {
            if offset < index + len {
                if offset == index {
                    break; // the terminator itself: end of this line
                }
                bail!("byte offset {offset} falls inside a line terminator");
            }
            line += 1;
            character = 0;
            index += len;
        } else {
            let ch = text[index..].chars().next().expect("char boundary checked");
            character += ch.len_utf16() as u32;
            index += ch.len_utf8();
        }
    }
    Ok(Position { line, character })
}

/// Convert an LSP position to a byte offset.
pub fn position_to_offset(text: &str, position: Position) -> Result<usize> {
    // Locate the start of `position.line`, recognizing every LSP line
    // terminator. Line 0 starts at byte 0; a text ending in a terminator has
    // a final empty line that is addressable (its start is the end of the
    // text).
    let mut line = 0u32;
    let mut line_start = 0usize;
    let mut index = 0usize;
    while line < position.line && index < text.len() {
        if let Some(len) = terminator_len(text, index) {
            line += 1;
            line_start = index + len;
            index += len;
        } else {
            index += text[index..]
                .chars()
                .next()
                .expect("char boundary")
                .len_utf8();
        }
    }
    if line < position.line {
        bail!(
            "line {} is past the end of the text ({line} is the last line)",
            position.line
        );
    }

    // Walk the line in UTF-16 units. A position names a boundary *between*
    // units, so one that points between the two halves of a surrogate pair
    // (e.g. column 2 of "x😀y") is rejected rather than silently resolved to
    // either half. A terminator ends the line: a column pointing at or past
    // it is past the end of the line.
    let mut character = 0u32;
    let mut offset = line_start;
    while offset < text.len() {
        if character == position.character {
            return Ok(offset);
        }
        if terminator_len(text, offset).is_some() {
            bail!(
                "column {} is past the end of line {} ({character} UTF-16 units long)",
                position.character,
                position.line
            );
        }
        let ch = text[offset..].chars().next().expect("char boundary");
        let width = ch.len_utf16() as u32;
        if position.character < character + width {
            bail!(
                "column {} points between the two halves of a surrogate pair (the character at column {character} is {width} UTF-16 units wide)",
                position.character
            );
        }
        character += width;
        offset += ch.len_utf8();
    }
    if character == position.character {
        return Ok(offset); // end of the line, or end of the text
    }
    bail!(
        "column {} is past the end of line {} ({character} UTF-16 units long)",
        position.character,
        position.line
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pos(line: u32, character: u32) -> Position {
        Position { line, character }
    }

    #[test]
    fn ascii_round_trips() {
        let text = "let config = Config::from_toml(&source)?;\nlet x = 1;\n";
        for offset in [0usize, 4, 12, 41, 42, 51, 53] {
            let position = offset_to_position(text, offset).unwrap();
            assert_eq!(position_to_offset(text, position).unwrap(), offset);
        }
    }

    #[test]
    fn non_ascii_round_trips_by_utf16_units() {
        // "aÄb中c😀d": a=1 byte, Ä=2 bytes (1..3), b=1 byte (3), 中=3 bytes
        // (4..7), c=1 byte (7), 😀=4 bytes (8..12), d=1 byte (12).
        let text = "aÄb中c😀d";
        assert_eq!(offset_to_position(text, 1).unwrap(), pos(0, 1)); // after 'a'
        assert_eq!(offset_to_position(text, 3).unwrap(), pos(0, 2)); // after 'Ä'
        assert_eq!(offset_to_position(text, 7).unwrap(), pos(0, 4)); // after 中, at 'c'
        assert_eq!(offset_to_position(text, 12).unwrap(), pos(0, 7)); // after 😀 (2 units), at 'd'
        // and back again
        for (column, byte) in [(1u32, 1usize), (2, 3), (3, 4), (4, 7), (5, 8), (7, 12)] {
            assert_eq!(position_to_offset(text, pos(0, column)).unwrap(), byte);
        }
        // byte offsets *inside* a multi-byte character are rejected.
        for offset in [2usize, 5, 6, 9, 10, 11] {
            let error = offset_to_position(text, offset).unwrap_err();
            assert!(error.to_string().contains("boundary"), "{error}");
        }
    }

    #[test]
    fn multi_line_offsets_count_newlines() {
        let text = "one\ntwo三家\nfour";
        // '三' on line 1: 3 UTF-8 bytes after 'w' 'o' → column 3
        let offset = text.find('三').unwrap();
        assert_eq!(offset_to_position(text, offset).unwrap(), pos(1, 3));
        assert_eq!(position_to_offset(text, pos(1, 3)).unwrap(), offset);
        // 'f' on line 2
        let offset = text.find('f').unwrap();
        assert_eq!(offset_to_position(text, offset).unwrap(), pos(2, 0));
        assert_eq!(position_to_offset(text, pos(2, 0)).unwrap(), offset);
    }

    #[test]
    fn end_of_text_is_a_valid_position() {
        let text = "abc\n";
        assert_eq!(offset_to_position(text, 4).unwrap(), pos(1, 0));
        assert_eq!(position_to_offset(text, pos(1, 0)).unwrap(), 4);
        assert_eq!(position_to_offset(text, pos(0, 3)).unwrap(), 3);
    }

    #[test]
    fn non_boundary_and_out_of_range_byte_offsets_are_rejected() {
        let text = "aÄb"; // bytes: a=0, Ä=1..3, b=3, len=4
        let error = offset_to_position(text, 2).unwrap_err();
        assert!(error.to_string().contains("boundary"), "{error}");
        // The end of the text is a valid offset; only *past* it is rejected.
        assert_eq!(offset_to_position(text, 4).unwrap(), pos(0, 3));
        let error = offset_to_position(text, 5).unwrap_err();
        assert!(error.to_string().contains("past the end"), "{error}");
    }

    #[test]
    fn line_past_the_end_is_rejected() {
        let text = "abc\ndef";
        let error = position_to_offset(text, pos(2, 0)).unwrap_err();
        assert!(error.to_string().contains("past the end"), "{error}");
    }

    #[test]
    fn column_past_the_line_end_is_rejected() {
        let text = "abc\ndef";
        let error = position_to_offset(text, pos(0, 4)).unwrap_err();
        assert!(
            error.to_string().contains("past the end of line 0"),
            "{error}"
        );
        // Past the end of a *later* line too — the newline must not be eaten.
        let error = position_to_offset(text, pos(1, 4)).unwrap_err();
        assert!(
            error.to_string().contains("past the end of line 1"),
            "{error}"
        );
    }

    #[test]
    fn crlf_is_a_terminator_not_content() {
        let text = "ab\r\ncd";
        // End-of-line positions in both directions; column 2 is the line end.
        assert_eq!(offset_to_position(text, 2).unwrap(), pos(0, 2));
        assert_eq!(position_to_offset(text, pos(0, 2)).unwrap(), 2);
        // The '\n' of the CRLF is inside a terminator — rejected, never
        // silently read as either end of line 0 or start of line 1.
        let error = offset_to_position(text, 3).unwrap_err();
        assert!(error.to_string().contains("line terminator"), "{error}");
        // A column pointing at the '\r' (column 3 of a 2-unit line) is past
        // the end of the line — the CR is not content.
        let error = position_to_offset(text, pos(0, 3)).unwrap_err();
        assert!(
            error.to_string().contains("past the end of line 0"),
            "{error}"
        );
        // The next line starts after the whole terminator.
        assert_eq!(offset_to_position(text, 4).unwrap(), pos(1, 0));
        assert_eq!(position_to_offset(text, pos(1, 0)).unwrap(), 4);
        assert_eq!(position_to_offset(text, pos(1, 1)).unwrap(), 5);
    }

    #[test]
    fn bare_cr_is_a_terminator_too() {
        let text = "ab\rcd";
        assert_eq!(offset_to_position(text, 2).unwrap(), pos(0, 2));
        assert_eq!(offset_to_position(text, 3).unwrap(), pos(1, 0));
        assert_eq!(position_to_offset(text, pos(1, 0)).unwrap(), 3);
        assert_eq!(position_to_offset(text, pos(1, 1)).unwrap(), 4);
    }

    #[test]
    fn surrogate_pair_boundaries() {
        // "x😀y" in UTF-16 units: [x][hi][lo][y]. Boundary 1 (before the
        // pair) and boundary 3 (after it) are legitimate; boundary 2, between
        // the two halves, is not a character boundary and is rejected.
        let text = "x😀y";
        assert_eq!(position_to_offset(text, pos(0, 1)).unwrap(), 1);
        let error = position_to_offset(text, pos(0, 2)).unwrap_err();
        assert!(error.to_string().contains("surrogate pair"), "{error}");
        assert_eq!(position_to_offset(text, pos(0, 3)).unwrap(), 5);
        // Round trip: the byte offset of 'y' maps back to column 3.
        assert_eq!(offset_to_position(text, 5).unwrap(), pos(0, 3));
    }
}
