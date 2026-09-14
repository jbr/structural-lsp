//! The `@decl` / `@expr` fixture annotations: parsing and resolving them
//! to positions in the source they annotate.

use super::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum AnnKind {
    Decl,
    Expr,
}

impl AnnKind {
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Decl => "decl",
            Self::Expr => "expr",
        }
    }
}

pub(super) struct Annotation {
    pub(super) kind: AnnKind,
    /// The symbol to resolve / query (the first field before `|`).
    pub(super) query: String,
    /// The literal source text to locate (the second field after `|`).
    pub(super) selection: String,
    /// Byte offset of the annotation line's start.
    pub(super) line_start: usize,
    /// Byte offset just past the annotation's own line: the selection must be
    /// found within `search_from..search_until`.
    pub(super) search_from: usize,
    /// Byte offset where this annotation's own region ends: the next
    /// annotation's line start, or end of file. Bounding the lookup here keeps
    /// a missing selection from silently matching a *later* annotation's text.
    pub(super) search_until: usize,
    pub(super) source_line: usize,
}

pub(super) struct Resolved {
    pub(super) selection_start: usize,
    pub(super) selection_end: usize,
    pub(super) target_offset: usize,
    /// How `target_offset` was chosen: `"query-in-selection"` when the query
    /// token was found inside the selection, `"selection-start"` when it was
    /// not — in which case the query is *not* confirmed to occur there, and the
    /// caller must read the provenance rather than assume the target is right.
    pub(super) target_provenance: &'static str,
}

/// Metadata for one annotation, written alongside every query it produced.
#[derive(Clone)]
pub(super) struct AnnotationMeta {
    pub(super) kind: &'static str,
    pub(super) query: String,
    pub(super) selection: String,
    pub(super) source_line: usize,
    pub(super) selection_range: Option<(Position, Position)>,
    pub(super) target: Option<Position>,
    pub(super) target_provenance: &'static str,
}

impl AnnotationMeta {
    pub(super) fn json(&self) -> Value {
        json!({
            "kind": self.kind,
            "query": self.query,
            "selection": self.selection,
            "source_line": self.source_line,
            "selection_range": self.selection_range,
            "target": self.target,
            "target_provenance": self.target_provenance,
        })
    }
}

pub(super) fn collect_sources(root: &Path) -> Result<Vec<(PathBuf, String, String)>> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).with_context(|| format!("reading {}", dir.display()))? {
            let path = entry?.path();
            if path.is_dir() {
                // Skip build output; never index it.
                if path.file_name().is_some_and(|n| n == "target") {
                    continue;
                }
                stack.push(path);
            } else if matches!(
                path.extension().and_then(|e| e.to_str()),
                Some("rs") | Some("py")
            ) {
                let text = fs::read_to_string(&path)?;
                let uri = url::Url::from_file_path(&path)
                    .map_err(|_| anyhow!("invalid file path {}", path.display()))?
                    .to_string();
                files.push((path, text, uri));
            }
        }
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(files)
}

pub(super) fn parse_annotations(text: &str) -> Vec<Annotation> {
    let mut out = Vec::new();
    let mut offset = 0usize;
    for (index, raw) in text.split_inclusive('\n').enumerate() {
        let line_start = offset;
        let line = raw.trim_end_matches(['\r', '\n']).trim_start();
        // Rust uses `//` comments, Python uses `#`; both carry the same annotation.
        let decl = line
            .strip_prefix("// @decl ")
            .or_else(|| line.strip_prefix("# @decl "));
        let expr = line
            .strip_prefix("// @expr ")
            .or_else(|| line.strip_prefix("# @expr "));
        let (kind, rest) = if let Some(rest) = decl {
            (AnnKind::Decl, rest)
        } else if let Some(rest) = expr {
            (AnnKind::Expr, rest)
        } else {
            offset += raw.len();
            continue;
        };
        if let Some((query, selection)) = rest.split_once('|') {
            let query = query.trim();
            let selection = selection.trim();
            if !query.is_empty() && !selection.is_empty() {
                out.push(Annotation {
                    kind,
                    query: query.to_string(),
                    selection: selection.to_string(),
                    line_start,
                    search_from: offset + raw.len(),
                    // Narrowed below, once every annotation's start is known.
                    search_until: text.len(),
                    source_line: index + 1,
                });
            }
        }
        offset += raw.len();
    }
    // An annotation's lookup region ends where the next annotation begins, so
    // its selection cannot be satisfied by a later annotation's source text.
    for index in 0..out.len() {
        out[index].search_until = out
            .get(index + 1)
            .map(|next| next.line_start)
            .unwrap_or(text.len());
    }
    out
}

/// Locate the annotation's `selection` within its own region (bounded above by
/// the next annotation), and the `query` token within that selection.
///
/// `None` means the selection text is not present in the region. If the query
/// token is not inside the selection, the target falls back to the selection
/// start with `target_provenance = "selection-start"`; it is never searched for
/// beyond the selection, which would silently resolve some later occurrence.
pub(super) fn resolve_annotation(text: &str, annotation: &Annotation) -> Option<Resolved> {
    let from = annotation.search_from.min(text.len());
    let until = annotation.search_until.clamp(from, text.len());
    let relative = text[from..until].find(&annotation.selection)?;
    let selection_start = from + relative;
    let selection_end = selection_start + annotation.selection.len();

    let (target_offset, target_provenance) = match annotation.selection.find(&annotation.query) {
        Some(relative) => (selection_start + relative, "query-in-selection"),
        None => (selection_start, "selection-start"),
    };

    Some(Resolved {
        selection_start,
        selection_end,
        target_offset,
        target_provenance,
    })
}

/// Byte offset to an LSP position. LSP positions are UTF-16 code units, which
/// only diverges from bytes on non-ASCII input; the fixtures are ASCII but the
/// conversion is done honestly.
pub(super) fn position(text: &str, offset: usize) -> Position {
    let mut line = 0u32;
    let mut character = 0u32;
    for (index, ch) in text.char_indices() {
        if index >= offset {
            break;
        }
        if ch == '\n' {
            line += 1;
            character = 0;
        } else {
            character += ch.len_utf16() as u32;
        }
    }
    Position { line, character }
}
