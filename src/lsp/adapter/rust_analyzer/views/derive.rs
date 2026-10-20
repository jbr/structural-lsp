//! What an empty `rust-analyzer/expandMacro` answer means when the focus is
//! inside a `#[derive(…)]` attribute (docs/servers.md).
//!
//! Measured against rust-analyzer 1.101.0-nightly (e9bbadf68 2026-10-12):
//! the request expands a proc-macro derive when the position is on a token
//! of its path inside the parentheses, and answers `null` everywhere else in
//! the attribute (on `#`, `[`, `derive`, `(`) and for every builtin derive
//! (`Clone`, `Copy`, `Debug`, `Default`, `Eq`, `Hash`, `Ord`, `PartialEq`,
//! `PartialOrd`) at any position. The generic "not a macro call" line is
//! wrong for all of those, so the attribute is recognized from the quoted
//! source and the line says which case it is.

use crate::lsp::adapter::ViewSite;

const BUILTIN_DERIVES: [&str; 9] = [
    "Clone",
    "Copy",
    "Debug",
    "Default",
    "Eq",
    "Hash",
    "Ord",
    "PartialEq",
    "PartialOrd",
];

/// The answer for an empty result at `site`, when the focus is in a derive
/// attribute; `None` when it is anywhere else.
pub fn explain_empty(site: ViewSite<'_>) -> Option<String> {
    let bytes = site.text.as_bytes();
    let offset = site.offset.min(bytes.len());
    // `#[` ending at or before the focus's first byte, or starting on it.
    let search_end = (offset + 2).min(bytes.len());
    let attr_start = bytes[..search_end].windows(2).rposition(|w| w == b"#[")?;
    let close = attr_start + bytes[attr_start..].iter().position(|&b| b == b']')?;
    if close < offset {
        return None;
    }
    let inner = std::str::from_utf8(&bytes[attr_start + 2..close]).ok()?;
    let list = inner.trim_start().strip_prefix("derive")?.trim_start();
    if !list.starts_with('(') {
        return None;
    }
    // The path under the focus, if the focus is inside the parentheses.
    let open = close - list.len();
    let name = (offset > open && offset < close)
        .then(|| path_at(&site.text[..close], offset))
        .flatten();
    let message = match name {
        Some(path) => {
            let last = path.rsplit("::").next().unwrap_or(path);
            if BUILTIN_DERIVES.contains(&last) {
                format!(
                    "no expansion: rust-analyzer returns none for the builtin derive `{path}`; \
                     it expands proc-macro derives only\n"
                )
            } else {
                format!("no expansion: rust-analyzer returned none for the derive `{path}`\n")
            }
        }
        None => "no expansion: rust-analyzer expands a derive at the derived trait's name \
                 inside the parentheses, not at `#[`, `derive` or the punctuation, and only a \
                 proc-macro derive (a builtin such as `Debug` or `Clone` has no expansion)\n"
            .to_string(),
    };
    Some(message)
}

/// The `a::b::C` path containing byte `offset`, or `None` when the byte is
/// not part of one.
fn path_at(text: &str, offset: usize) -> Option<&str> {
    let bytes = text.as_bytes();
    let part = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b':';
    if offset >= bytes.len() || !part(bytes[offset]) {
        return None;
    }
    let start = bytes[..offset]
        .iter()
        .rposition(|&b| !part(b))
        .map_or(0, |i| i + 1);
    let end = bytes[offset..]
        .iter()
        .position(|&b| !part(b))
        .map_or(bytes.len(), |i| offset + i);
    Some(&text[start..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(text: &str, needle: &str, within: usize) -> Option<String> {
        let offset = text.find(needle).expect("needle") + within;
        explain_empty(ViewSite { text, offset })
    }

    const SOURCE: &str = "#[derive(Debug, pm::Hello)]\npub struct S;\n";

    #[test]
    fn a_builtin_derive_name_says_builtin() {
        let message = at(SOURCE, "Debug", 2).unwrap();
        assert!(message.contains("builtin derive `Debug`"), "{message}");
    }

    #[test]
    fn a_proc_macro_derive_name_with_no_answer_names_the_path() {
        let message = at(SOURCE, "Hello", 1).unwrap();
        assert_eq!(
            message,
            "no expansion: rust-analyzer returned none for the derive `pm::Hello`\n"
        );
    }

    #[test]
    fn the_keyword_and_punctuation_say_where_derives_expand() {
        for (needle, within) in [("#[", 0), ("derive", 0), ("derive", 3), ("(", 0)] {
            let message = at(SOURCE, needle, within).unwrap();
            assert!(
                message.contains("expands a derive at the derived trait's name"),
                "{needle}: {message}"
            );
        }
    }

    #[test]
    fn a_derive_spread_over_lines_is_still_one_attribute() {
        let text = "#[derive(\n    Clone,\n    Serialize,\n)]\nstruct S;\n";
        let message = at(text, "Serialize", 0).unwrap();
        assert!(message.contains("`Serialize`"), "{message}");
    }

    #[test]
    fn other_places_are_not_derives() {
        let text = "#[derive(Debug)]\nstruct S;\n#[cfg(test)]\nfn f() {}\nlet x = 1;\n";
        assert_eq!(at(text, "struct", 0), None);
        assert_eq!(at(text, "cfg", 0), None);
        assert_eq!(at(text, "let", 0), None);
        assert_eq!(at("#[cfg(derive)]\n", "derive", 0), None);
    }
}
