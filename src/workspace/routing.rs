//! Routing one call to servers: the single rule that both what starts and
//! what is asked go through, the argument errors a `language` can be, and
//! what a call is told when the rule selects nothing running
//! (`docs/ownership.md`).
//!
//! Everything here reads slots — the plans a tree called for — and never
//! asks which servers happen to be running before it decides what a call
//! may say. That is what keeps an argument valid before a startup
//! completes valid after it.

use super::{ServerSlot, Workspace};
use crate::discovery::selection::DeclinedReason;
use crate::lsp::adapter::language_for;
use anyhow::{Result, bail};
use std::path::Path;
use std::sync::Arc;

#[cfg(test)]
mod tests;

/// The slots that would serve a call scoped to `scope` and narrowed to
/// `language`: by the scope's file first, then by the language.
///
/// One rule, in one place, because two callers need it for different reasons
/// — a launch asks which servers to start, a query asks which to send to —
/// and they must never disagree about it.
///
/// - a scope naming an existing file selects the slots whose adapter knows
///   that file's language;
/// - anything else — no scope, a directory, a path that is not a file —
///   selects every slot;
/// - a file with no extension that matches nothing selects every slot,
///   since there is nothing to route it by;
/// - a `language` then keeps the slots one of whose languages answers to
///   it ([`crate::lsp::adapter::Language::answers_to`]).
pub fn serving(
    slots: &[Arc<ServerSlot>],
    scope: Option<&Path>,
    language: Option<&str>,
) -> Vec<usize> {
    by_scope(slots, scope)
        .into_iter()
        .filter(|&index| language.is_none_or(|asked| speaks(&slots[index], asked)))
        .collect()
}

/// The scope half of [`serving`].
fn by_scope(slots: &[Arc<ServerSlot>], scope: Option<&Path>) -> Vec<usize> {
    let all = || (0..slots.len()).collect::<Vec<usize>>();
    let Some(scope) = scope.filter(|scope| scope.is_file()) else {
        return all();
    };
    let serving: Vec<usize> = (0..slots.len())
        .filter(|index| language_for(&slots[*index].adapter().languages(), scope).is_some())
        .collect();
    if serving.is_empty() && scope.extension().is_none() {
        // A file of no known language and no extension leaves nothing to
        // route by, so every server is a candidate, as it always was.
        return all();
    }
    serving
}

/// Whether one of `slot`'s languages answers to `asked`.
fn speaks(slot: &ServerSlot, asked: &str) -> bool {
    slot.adapter()
        .languages()
        .iter()
        .any(|language| language.answers_to(asked))
}

impl Workspace {
    /// The argument errors a `language` can be: one no slot here speaks,
    /// or one a file `path` contradicts. Always `Ok` without a language.
    ///
    /// Decided against the slots, never against what is running: a call
    /// that is valid while a server is still starting stays valid once it
    /// has started (`docs/ownership.md`). A tree that grows a language
    /// lays out a slot for it, so a language refused here can become
    /// valid; one accepted here cannot become invalid, because slots are
    /// never removed.
    pub fn check_language(&self, scope: Option<&Path>, language: Option<&str>) -> Result<()> {
        let Some(asked) = language else {
            return Ok(());
        };
        if self.config_error.is_some() {
            // Nothing was configured, so no language could be matched; the
            // configuration is what to fix.
            bail!("{}", self.nothing_running());
        }
        let speaking: Vec<usize> = (0..self.slots.len())
            .filter(|&index| speaks(&self.slots[index], asked))
            .collect();
        if speaking.is_empty() {
            bail!(
                "`language: {asked}` matches no language server here; languages here: {}\n{}",
                self.languages(),
                self.not_here_for(asked)
            );
        }
        if let Some(file) = scope.filter(|scope| scope.is_file()) {
            let by_file = by_scope(&self.slots, Some(file));
            // A file no slot serves is the query's to report, with or
            // without a language; this is only the contradiction.
            if !by_file.is_empty() && !by_file.iter().any(|index| speaking.contains(index)) {
                bail!(
                    "`path` is a `{}` file, which `language: {asked}` does not cover: here it is \
                     served by {}. Drop `language`, or name a file in that language.",
                    file.extension().unwrap_or_default().to_string_lossy(),
                    by_file
                        .iter()
                        .map(|&index| self.slots[index].name())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
        }
        Ok(())
    }

    /// The running slots a query should go to: [`serving`]'s answer,
    /// narrowed to the ones running. An actionable error when none of them
    /// is running, naming what each would-be server is doing instead.
    pub fn select(&self, scope: Option<&Path>, language: Option<&str>) -> Result<Vec<usize>> {
        self.check_language(scope, language)?;
        let candidates = serving(&self.slots, scope, language);
        if candidates.is_empty() {
            // A config that will not parse means nothing was configured, so
            // the answer is about the file rather than about this scope.
            if self.config_error.is_some() {
                bail!("{}", self.nothing_running());
            }
            let extension = scope
                .filter(|scope| scope.is_file())
                .and_then(Path::extension);
            let Some(extension) = extension else {
                // With no file extension to route by, every slot is a
                // candidate, so none is empty only when nothing was laid
                // out: the tree calls for no language this catalogue serves.
                bail!(
                    "no language server is set up here: nothing in this tree calls for one.\n{}",
                    self.not_planned()
                );
            };
            bail!(
                "no language server here serves `{}` files.\n  here: {}\n{}",
                extension.to_string_lossy(),
                self.names(),
                self.not_planned()
            );
        }
        let running = self.running_of(&candidates);
        if running.is_empty() {
            // A file or a language names the servers that should have
            // served it; a bare scope has nothing to name but the whole
            // configuration.
            if language.is_some() || scope.is_some_and(|scope| scope.is_file()) {
                bail!("{}", self.servers_down(&candidates));
            }
            bail!("{}", self.nothing_running());
        }
        Ok(running)
    }

    /// Which of `candidates` have a server, read from the workspace's own
    /// copy: while this lock is held, a server in `servers` *is* a running
    /// server. The slot's state is for readers that hold no workspace lock.
    fn running_of(&self, candidates: &[usize]) -> Vec<usize> {
        candidates
            .iter()
            .copied()
            .filter(|index| self.servers[*index].is_some())
            .collect()
    }

    /// Every language a slot here speaks, once each, with its extensions:
    /// what a `language` argument may name.
    fn languages(&self) -> String {
        let mut seen: Vec<String> = Vec::new();
        let mut listed: Vec<String> = Vec::new();
        for slot in &self.slots {
            for language in slot.adapter().languages() {
                if seen.contains(&language.id) {
                    continue;
                }
                listed.push(format!(
                    "{} ({})",
                    language.id,
                    language
                        .extensions
                        .iter()
                        .map(|extension| format!(".{extension}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
                seen.push(language.id);
            }
        }
        if listed.is_empty() {
            return "none".to_string();
        }
        listed.join(", ")
    }

    /// Why nothing here speaks `asked`, when selection knows: a server for
    /// it was left out because nothing in the tree calls for it, or the
    /// tree has it and nothing serves it.
    fn not_here_for(&self, asked: &str) -> String {
        let Some(selection) = &self.selection else {
            return String::new();
        };
        let declined = selection.declined.iter().filter(|declined| {
            matches!(&declined.reason, DeclinedReason::NoEvidence { languages }
                if languages.iter().any(|language| language.eq_ignore_ascii_case(asked)))
        });
        let unserved = selection
            .unserved
            .iter()
            .filter(|unserved| unserved.language.eq_ignore_ascii_case(asked));
        declined
            .map(|declined| format!("  not here: {}\n", declined.explain()))
            .chain(unserved.map(|unserved| format!("  not here: {}\n", unserved.explain())))
            .collect()
    }

    /// The servers this workspace has no slot for, and why selection left
    /// each one out.
    ///
    /// Without this a configured server simply vanishes from the message.
    /// The case is ordinary: a tree with no Python lays out no Python
    /// slot, and then a `.py` file appears — the file is real, `pyright`
    /// is configured, and "no language server serves `py` files" would be
    /// true and useless. Noticing the new language while the workspace is
    /// live is observation's
    /// (`docs/observation.md`); saying so is this.
    fn not_planned(&self) -> String {
        let Some(selection) = &self.selection else {
            return String::new();
        };
        selection
            .declined
            .iter()
            .map(|declined| format!("  not here: {}\n", declined.explain()))
            .collect()
    }

    fn names(&self) -> String {
        self.slots
            .iter()
            .map(|slot| slot.name())
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// What to say when the servers that would serve a call are not
    /// running: each one's state, and how to fix the ones that failed.
    fn servers_down(&self, candidates: &[usize]) -> String {
        let mut text = String::new();
        for &index in candidates {
            let slot = &self.slots[index];
            let state = slot.state();
            text.push_str(&format!("  {}: {}", slot.name(), state.label()));
            if let Some(reason) = state.reason() {
                text.push_str(&format!(
                    ": {reason}; to fix: {}",
                    slot.adapter().install_hint()
                ));
            }
            text.push('\n');
            text.push_str(&tried(slot));
        }
        text
    }

    fn nothing_running(&self) -> String {
        let mut text = String::from("no language server is running.\n");
        if let Some(error) = &self.config_error {
            text.push_str(&format!("  configuration: {error}\n"));
        }
        for slot in &self.slots {
            let state = slot.state();
            text.push_str(&format!("  {}: {}", slot.name(), state.label()));
            if let Some(reason) = state.reason() {
                text.push_str(&format!(
                    "; to fix: {}",
                    slot.adapter().install_hint().as_str()
                ));
                text.push_str(&format!("\n    {reason}"));
            }
            text.push('\n');
            text.push_str(&tried(slot));
        }
        text.push_str("Restart this server after installing or configuring one.");
        text
    }
}

/// The candidates a slot tried and rejected, indented under it. Empty for a
/// slot that never fell back, so an ordinary failure reads as it always did.
fn tried(slot: &ServerSlot) -> String {
    slot.failed_attempts()
        .iter()
        .map(|attempt| format!("    tried {}\n", attempt.explain()))
        .collect()
}
