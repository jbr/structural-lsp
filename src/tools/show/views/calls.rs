//! `view = "incoming-calls"` / `"outgoing-calls"`: the call hierarchy at
//! the target, one line per caller or callee with its call sites under it.

use super::unsupported;
use crate::lsp::definition::candidate_path;
use crate::lsp::positions::Encoding;
use crate::workspace::Workspace;
use crate::tools::hits::source_line;
use crate::tools::listing::{header, location_line};
use crate::tools::resolution::{Resolved, Target, resolve_target, server_named, sync_target};
use anyhow::{Context, Result};
use lsp_types::{CallHierarchyItem, Range};
use serde::Deserialize;
use serde_json::Value;
use std::path::Path;

/// One direction's entry, as the spec spells both: the other item and the
/// ranges of the calls, which lie in the caller's file.
#[derive(Deserialize)]
struct Call {
    #[serde(alias = "from", alias = "to")]
    item: CallHierarchyItem,
    #[serde(rename = "fromRanges")]
    from_ranges: Vec<Range>,
}

pub(super) fn run(state: &mut Workspace, root: &Path, target: Target<'_>, incoming: bool) -> Result<String> {
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
    let prepared = server.prepare_call_hierarchy(&synced.uri, found.target.selection_range.start)?;
    let items: Vec<CallHierarchyItem> = match prepared {
        Value::Null => Vec::new(),
        other => serde_json::from_value(other)
            .with_context(|| format!("{} returned an unrecognizable prepareCallHierarchy result", server.name()))?,
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
        let batch: Vec<Call> = serde_json::from_value(raw)
            .with_context(|| format!("{} returned an unrecognizable call hierarchy result", server.name()))?;
        calls.extend(batch);
    }
    let noun = if incoming { "caller" } else { "callee" };
    if calls.is_empty() {
        return Ok(format!("{head} · no {noun}s\n"));
    }
    calls.sort_by(|a, b| {
        (a.item.uri.as_str(), a.item.selection_range.start.line)
            .cmp(&(b.item.uri.as_str(), b.item.selection_range.start.line))
    });
    let mut text = format!(
        "{head} · {} {noun}{}\n",
        calls.len(),
        if calls.len() == 1 { "" } else { "s" }
    );
    // Call sites are in the caller's file: the other item's for incoming
    // calls, the target's for outgoing ones.
    let target_text = synced.text;
    let encoding = server.encoding();
    let mut budget = super::MAX_LINES;
    for call in &calls {
        if budget == 0 {
            break;
        }
        budget -= 1;
        text.push_str(&header(
            Some(server.adapter().kind_label(call.item.kind)),
            &call.item.name,
            &location_line(root, &call.item.uri, call.item.selection_range.start),
        ));
        text.push('\n');
        let site_text = if incoming {
            candidate_path(&call.item.uri).and_then(|path| std::fs::read_to_string(path).ok())
        } else {
            Some(target_text.clone())
        };
        let Some(site_text) = site_text else {
            continue;
        };
        let mut sites = call.from_ranges.clone();
        sites.sort_by_key(|range| (range.start.line, range.start.character));
        for site in sites {
            if budget == 0 {
                break;
            }
            budget -= 1;
            text.push_str(&format!(
                "  {} · {}\n",
                site.start.line + 1,
                site_line(&site_text, site, encoding)
            ));
        }
    }
    if budget == 0 {
        text.push_str(&format!("note: listing cut at {} lines\n", super::MAX_LINES));
    }
    Ok(text)
}

fn site_line(text: &str, range: Range, encoding: Encoding) -> String {
    source_line(text, range.start, encoding)
}
