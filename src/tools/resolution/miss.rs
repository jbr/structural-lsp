//! What a total miss says beyond "not found": the servers that were asked
//! and have answered nothing all session, and the languages under a
//! directory scope that no asked server serves (docs/resolution.md).

use crate::discovery::evidence::Sources;
use crate::lsp::server::LspServer;
use crate::tools::listing::{directory_scope_note, unanswered_note};
use crate::workspace::Workspace;
use std::path::Path;

/// Which extensions live under a directory scope that found nothing, and
/// which of them no running server serves.
///
/// A directory scope asks every running server by design
/// (`Workspace::select`), so a miss over an unserved tree reads as an
/// absent name. The scan is the discovery budget walk (`Sources::scan`):
/// presence claims hold under truncation, and build and hidden directories
/// are skipped. Served-ness is what the query observed — running servers —
/// rather than what is configured: a configured server that is down serves
/// nothing either, and the note names running servers so the two cases read
/// the same way (`docs/resolution.md`). Only the servers asked: a
/// `language` narrows them, and a server left out by it served nothing
/// here either.
pub(super) fn directory_languages_note(
    state: &Workspace,
    selected: &[usize],
    root: &Path,
    dir: &Path,
) -> String {
    let sources = Sources::scan(dir);
    let contained: Vec<String> = sources.by_extension.keys().cloned().collect();
    if contained.is_empty() {
        return String::new();
    }
    let running: Vec<&LspServer> = selected
        .iter()
        .filter_map(|&index| state.server(index).ok())
        .collect();
    let serves = |extension: &str| {
        running.iter().any(|server| {
            server
                .adapter()
                .languages()
                .iter()
                .flat_map(|language| language.extensions.iter())
                .any(|known| known.eq_ignore_ascii_case(extension))
        })
    };
    let unserved: Vec<String> = contained
        .iter()
        .filter(|extension| !serves(extension))
        .cloned()
        .collect();
    let display = dir
        .strip_prefix(root)
        .map(|relative| relative.display().to_string())
        .ok()
        .filter(|relative| !relative.is_empty())
        .unwrap_or_else(|| dir.display().to_string());
    let names = running
        .iter()
        .map(|server| server.name())
        .collect::<Vec<_>>()
        .join(", ");
    directory_scope_note(&format!("{display}/"), &contained, &unserved, &names)
}

/// The asked servers behind a total miss that advertised an index yet have
/// answered nothing all session.
///
/// Only servers that were asked and could have answered: one with no
/// workspace index is unserved rather than silent, and a server that yielded
/// once — from its index or from an outline — has shown it is working
/// (docs/resolution.md).
pub(super) fn unanswered_note_for(state: &Workspace, selected: &[usize]) -> String {
    let mut servers = Vec::new();
    let mut languages = Vec::new();
    for &index in selected {
        let Ok(server) = state.server(index) else {
            continue;
        };
        if !server.supports("workspaceSymbolProvider") || server.has_answered() {
            continue;
        }
        servers.push(server.name().to_string());
        for language in server.adapter().languages() {
            if !languages.contains(&language.id) {
                languages.push(language.id);
            }
        }
    }
    unanswered_note(&servers, &languages)
}
