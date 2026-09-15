//! `textDocument/references` results as pure data: normalised, the
//! declaration dropped, hits outside the root counted, the rest grouped by
//! file in a stable order. No I/O; the tool layer syncs and verifies.

use crate::lsp::positions::{Encoding, position_to_offset};
use anyhow::{Context, Result, bail};
use lsp_types::{Location, Range, Uri};
use serde_json::Value;
use std::path::{Path, PathBuf};

/// One reference the server reported, with the canonical path of its file.
#[derive(Debug, Clone, PartialEq)]
pub struct Hit {
    pub uri: Uri,
    pub range: Range,
}

/// The hits of one file, in the server's order.
#[derive(Debug, Clone, PartialEq)]
pub struct FileHits {
    pub path: PathBuf,
    pub uri: Uri,
    pub hits: Vec<Hit>,
}

/// What a references result says once the declaration and anything outside
/// the root are set aside.
#[derive(Debug, Default, PartialEq)]
pub struct Grouped {
    /// Files in first-appearance order; each file's hits in the server's
    /// order. Sorting is a rendering decision.
    pub files: Vec<FileHits>,
    /// Hits whose file lies outside the workspace root, or is not a file.
    pub out_of_root: usize,
    /// Hits that were the declaration itself despite `includeDeclaration:
    /// false`.
    pub declaration: usize,
}

impl Grouped {
    pub fn total(&self) -> usize {
        self.files.iter().map(|file| file.hits.len()).sum()
    }
}

/// Normalise a `textDocument/references` result: `null` or `[]` is no
/// references; otherwise `Location[]`. Anything else is an error carrying
/// the raw value, never a guess.
pub fn parse_references_result(value: Value) -> Result<Vec<Hit>> {
    let items = match value {
        Value::Null => return Ok(Vec::new()),
        Value::Array(items) => items,
        other => bail!("references result was neither null nor an array: {other}"),
    };
    items
        .into_iter()
        .map(|item| {
            let location: Location = serde_json::from_value(item.clone())
                .with_context(|| format!("parsing Location in references result item {item}"))?;
            Ok(Hit {
                uri: location.uri,
                range: location.range,
            })
        })
        .collect()
}

/// Group hits by file under the canonical `root`, dropping the declaration
/// (`declaration_uri` and `declaration_range`, compared as the server
/// spells them) and counting what lies outside the root. Files are
/// canonicalised when they exist so symlinks resolve before the root check.
pub fn group_by_file(
    hits: Vec<Hit>,
    root: &Path,
    declaration_uri: &Uri,
    declaration_range: Range,
) -> Grouped {
    let mut grouped = Grouped::default();
    for hit in hits {
        if &hit.uri == declaration_uri && hit.range == declaration_range {
            grouped.declaration += 1;
            continue;
        }
        let path = crate::lsp::definition::candidate_path(&hit.uri)
            .map(|path| std::fs::canonicalize(&path).unwrap_or(path));
        let Some(path) = path.filter(|path| path == root || path.starts_with(root)) else {
            grouped.out_of_root += 1;
            continue;
        };
        match grouped.files.iter_mut().find(|file| file.path == path) {
            Some(file) => file.hits.push(hit),
            None => grouped.files.push(FileHits {
                path,
                uri: hit.uri.clone(),
                hits: vec![hit],
            }),
        }
    }
    grouped
}

/// Check that the text at `range` is `name` or one of the server's
/// reference `aliases` (rust-analyzer reports `Self` as a reference to the
/// type). Anything else — including an empty range — means the server's
/// range no longer matches the file, and the caller reports that rather
/// than listing it.
pub fn verify_reference_at(
    text: &str,
    range: Range,
    name: &str,
    aliases: &[&str],
    encoding: Encoding,
) -> Result<()> {
    let start = position_to_offset(text, range.start, encoding)
        .context("the reported range starts outside the file as read from disk")?;
    let end = position_to_offset(text, range.end, encoding)
        .context("the reported range ends outside the file as read from disk")?;
    if end < start {
        bail!("the reported range is inverted");
    }
    let found = &text[start..end];
    if found == name || aliases.contains(&found) {
        Ok(())
    } else {
        bail!(
            "expected `{name}` at {}:{}, found `{found}`",
            range.start.line + 1,
            range.start.character + 1
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn uri(path: &str) -> Uri {
        serde_json::from_value(json!(format!("file://{path}"))).unwrap()
    }

    fn range(line: u32, start: u32, end: u32) -> Range {
        Range::new(
            lsp_types::Position::new(line, start),
            lsp_types::Position::new(line, end),
        )
    }

    fn location(path: &str, line: u32) -> Value {
        json!({
            "uri": format!("file://{path}"),
            "range": { "start": { "line": line, "character": 4 }, "end": { "line": line, "character": 10 } }
        })
    }

    #[test]
    fn null_and_empty_are_no_references_and_garbage_is_an_error() {
        assert!(parse_references_result(Value::Null).unwrap().is_empty());
        assert!(parse_references_result(json!([])).unwrap().is_empty());
        assert!(parse_references_result(json!({"uri": "x"})).is_err());
        assert!(parse_references_result(json!([{"range": 1}])).is_err());
    }

    #[test]
    fn a_reference_holds_the_name_or_an_alias_and_nothing_else() {
        let text = "impl Config {\n    fn new() -> Self {\n";
        let ok = verify_reference_at(text, range(0, 5, 11), "Config", &[], Encoding::Utf8);
        assert!(ok.is_ok());
        let alias =
            verify_reference_at(text, range(1, 16, 20), "Config", &["Self"], Encoding::Utf8);
        assert!(alias.is_ok());
        let no_alias = verify_reference_at(text, range(1, 16, 20), "Config", &[], Encoding::Utf8);
        assert!(no_alias.unwrap_err().to_string().contains("found `Self`"));
        let empty = verify_reference_at(text, range(0, 5, 5), "Config", &[], Encoding::Utf8);
        assert!(empty.is_err(), "an empty range names nothing");
        assert!(verify_reference_at(text, range(9, 0, 1), "Config", &[], Encoding::Utf8).is_err());
    }

    #[test]
    fn hits_are_grouped_by_file_in_first_appearance_order() {
        let root = std::env::temp_dir().canonicalize().unwrap();
        let a = root.join("structural-refs-a.rs");
        let b = root.join("structural-refs-b.rs");
        std::fs::write(&a, "").unwrap();
        std::fs::write(&b, "").unwrap();
        let (a_str, b_str) = (a.to_str().unwrap(), b.to_str().unwrap());
        let hits = parse_references_result(json!([
            location(b_str, 3),
            location(a_str, 1),
            location(b_str, 9),
            location("/definitely/outside/root.rs", 0),
            location(a_str, 0),
        ]))
        .unwrap();
        let grouped = group_by_file(hits, &root, &uri(a_str), range(0, 4, 10));
        assert_eq!(grouped.declaration, 1, "the declaration hit is set aside");
        assert_eq!(grouped.out_of_root, 1);
        assert_eq!(grouped.total(), 3);
        let order: Vec<(&Path, usize)> = grouped
            .files
            .iter()
            .map(|file| (file.path.as_path(), file.hits.len()))
            .collect();
        assert_eq!(order, [(b.as_path(), 2), (a.as_path(), 1)]);
        let _ = std::fs::remove_file(a);
        let _ = std::fs::remove_file(b);
    }
}
