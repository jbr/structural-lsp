//! `view = "incoming-calls"` / `"outgoing-calls"`: the call hierarchy at
//! the target, one line per caller or callee with its call sites under it.
//!
//! Entries fall into three sections, in this order: the workspace's own
//! code, its tests (the classifier `references` uses), and code outside
//! the workspace root — a callee in a dependency or the standard library
//! for outgoing calls, a caller in another package for incoming ones
//! (docs/tools.md). Rendering is in `calls/render.rs`.

use super::unsupported;
use crate::lsp::definition::candidate_path;
use crate::lsp::outline;
use crate::lsp::server::LspServer;
use crate::tools::classify;
use crate::tools::hits::{OutlineGap, source_line};
use crate::tools::listing::location_line;
use crate::tools::resolution::{Resolved, Target, resolve_target, server_named, sync_target};
use crate::workspace::Workspace;
use anyhow::{Context, Result};
use lsp_types::{CallHierarchyItem, Range};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;

mod render;
use render::{Calls, Entry, Site, render_calls};

/// One direction's entry, as the spec spells both: the other item and the
/// ranges of the calls, which lie in the caller's file.
#[derive(Deserialize)]
struct Call {
    #[serde(alias = "from", alias = "to")]
    item: CallHierarchyItem,
    #[serde(rename = "fromRanges")]
    from_ranges: Vec<Range>,
}

pub(super) fn run(
    state: &mut Workspace,
    root: &Path,
    target: Target<'_>,
    incoming: bool,
) -> Result<String> {
    let view = if incoming {
        super::INCOMING_CALLS
    } else {
        super::OUTGOING_CALLS
    };
    let found = match resolve_target(state, target)? {
        Resolved::Listed(listing) => return Ok(listing),
        Resolved::One(found) => *found,
    };
    let head = super::target_header(state, root, &found)?;
    let (_, synced) = sync_target(state, root, &found)?;
    let server = server_named(state, &found.identity.server)?;
    if !server.supports("callHierarchyProvider") {
        return Ok(unsupported(view, server.name()));
    }
    server.wait_ready()?;
    let prepared =
        server.prepare_call_hierarchy(&synced.uri, found.target.selection_range.start)?;
    let items: Vec<CallHierarchyItem> = match prepared {
        Value::Null => Vec::new(),
        other => serde_json::from_value(other).with_context(|| {
            format!(
                "{} returned an unrecognizable prepareCallHierarchy result",
                server.name()
            )
        })?,
    };
    if items.is_empty() {
        return Ok(format!(
            "{head}\nnot applicable: `{}` is not a callable the server can walk calls for\n",
            found.name
        ));
    }
    let mut calls: Vec<Call> = Vec::new();
    for item in &items {
        let raw = server.calls(incoming, &serde_json::to_value(item)?)?;
        if raw.is_null() {
            continue;
        }
        let batch: Vec<Call> = serde_json::from_value(raw).with_context(|| {
            format!(
                "{} returned an unrecognizable call hierarchy result",
                server.name()
            )
        })?;
        calls.extend(batch);
    }
    calls.sort_by(|a, b| {
        (a.item.uri.as_str(), a.item.selection_range.start.line)
            .cmp(&(b.item.uri.as_str(), b.item.selection_range.start.line))
    });

    let mut sections = Calls {
        head: &head,
        incoming,
        total: calls.len(),
        project: Vec::new(),
        tests: Vec::new(),
        outside: Vec::new(),
        notes: Vec::new(),
    };
    let mut places = Places {
        server,
        root,
        outlines: HashMap::new(),
        gap: None,
    };
    // Call sites are in the caller's file: the other item's for incoming
    // calls, the target's for outgoing ones.
    let target_text = synced.text;
    let mut outside_order = Vec::new();
    for call in calls {
        let site_text = if incoming {
            candidate_path(&call.item.uri).and_then(|path| std::fs::read_to_string(path).ok())
        } else {
            Some(target_text.clone())
        };
        let entry = entry(server, root, &call, site_text.as_deref());
        match places.place_of(&call.item)? {
            Place::Project => sections.project.push(entry),
            Place::Test => sections.tests.push(entry),
            Place::Outside => {
                let first = call.from_ranges.iter().map(|range| range.start).min();
                outside_order.push((first.map(|start| (start.line, start.character)), entry));
            }
        }
    }
    // An outside callee is a line of the target's body, so its rows go in
    // the order the body first calls each; an outside caller keeps path
    // order with the rest.
    if !incoming {
        outside_order.sort_by_key(|(first, _)| *first);
    }
    sections.outside = outside_order.into_iter().map(|(_, entry)| entry).collect();
    if let Some(gap) = &places.gap {
        sections.notes.push(gap_note(server.name(), gap));
    }
    Ok(render_calls(sections))
}

/// A call as it is printed: the other item and the call sites, sorted.
fn entry(server: &LspServer, root: &Path, call: &Call, site_text: Option<&str>) -> Entry {
    let mut ranges = call.from_ranges.clone();
    ranges.sort_by_key(|range| (range.start.line, range.start.character));
    let sites = ranges
        .iter()
        .map(|range| Site {
            line: range.start.line + 1,
            text: site_text.map(|text| source_line(text, range.start, server.encoding())),
        })
        .collect();
    Entry {
        kind: server.adapter().kind_label(call.item.kind).to_string(),
        name: call.item.name.clone(),
        location: location_line(root, &call.item.uri, call.item.selection_range.start),
        sites,
    }
}

/// Which section an entry belongs in.
enum Place {
    Project,
    Test,
    Outside,
}

/// Classifies call hierarchy items, outlining each file under the root
/// once.
struct Places<'a> {
    server: &'a LspServer,
    root: &'a Path,
    /// By URI; `None` when the server gave no outline for the file.
    outlines: HashMap<String, Option<Value>>,
    /// The first reason an outline was missing.
    gap: Option<OutlineGap>,
}

impl Places<'_> {
    /// Outside when the item's file is not under the root (symlinks
    /// resolved first, as `references` resolves them); a test when its file
    /// is a test file or the chain enclosing its name is a test module.
    fn place_of(&mut self, item: &CallHierarchyItem) -> Result<Place> {
        let path = candidate_path(&item.uri).map(|path| std::fs::canonicalize(&path).unwrap_or(path));
        let Some(path) = path.filter(|path| path.starts_with(self.root)) else {
            return Ok(Place::Outside);
        };
        if classify::path_is_test(&path) {
            return Ok(Place::Test);
        }
        let key = item.uri.as_str().to_string();
        if !self.outlines.contains_key(&key) {
            let outline = match self.server.document_symbol(&key) {
                Ok(Some(outline)) => Some(outline),
                Ok(None) => {
                    self.gap.get_or_insert(OutlineGap::Unsupported);
                    None
                }
                Err(error) => {
                    let why = format!("{error:#}").lines().next().unwrap_or("").to_string();
                    self.gap.get_or_insert(OutlineGap::Refused(why));
                    None
                }
            };
            self.outlines.insert(key.clone(), outline);
        }
        let Some(Some(outline)) = self.outlines.get(&key) else {
            return Ok(Place::Project);
        };
        let chain = outline::chain_at(outline, item.selection_range.start).with_context(|| {
            format!(
                "{} returned an unusable documentSymbol result",
                self.server.name()
            )
        })?;
        Ok(if classify::chain_is_test(&chain) {
            Place::Test
        } else {
            Place::Project
        })
    }
}

/// Without an outline, an entry is a test only by its file's path.
fn gap_note(server: &str, gap: &OutlineGap) -> String {
    let why = match gap {
        OutlineGap::Unsupported => "does not support documentSymbol".to_string(),
        OutlineGap::Refused(why) => format!("would not outline every file listed ({why})"),
    };
    format!("{server} {why}; tests there are recognized by their path alone")
}
