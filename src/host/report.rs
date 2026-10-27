//! What an entry tells the operator, and when (`docs/operator.md`).
//!
//! Every method here is called at a transition — a workspace built, a
//! launch ended, a refresh that changed the tree's languages, a sweep that
//! stopped something — and never per query. What a query's caller can act
//! on is in its answer; these are the facts left over.

use super::Entry;
use crate::discovery::selection::Unserved;
use crate::lsp::server::LspServer;
use crate::operator::{Event, Kind, Level};
use crate::workspace::{Attempt, ServerSlot};
use std::sync::PoisonError;

impl Entry {
    /// Hand one event to the operator, attributed to this workspace.
    pub(crate) fn report(&self, level: Level, kind: Kind, server: Option<&str>, message: String) {
        let mut event = Event::new(level, kind, &self.root, message);
        if let Some(server) = server {
            event = event.with_server(server);
        }
        self.operator.event(&event);
    }

    /// What building the workspace found: a configuration that does not
    /// load, or languages the tree holds that nothing serves.
    pub(super) fn report_built(&self) {
        let (config_error, unserved) = {
            let workspace = self.lock();
            (
                workspace.config_error().map(str::to_string),
                workspace
                    .selection()
                    .map(|selection| selection.unserved.clone())
                    .unwrap_or_default(),
            )
        };
        if let Some(error) = config_error {
            self.report(
                Level::Error,
                Kind::Config,
                None,
                format!(
                    "the configuration did not load, so no language server starts here: {error}"
                ),
            );
        }
        self.report_unserved(&unserved);
    }

    /// Languages present with no server, each reported once per workspace:
    /// at the first selection that finds it and never again, however often
    /// a refresh finds it still there.
    pub(crate) fn report_unserved(&self, unserved: &[Unserved]) {
        for language in unserved {
            let first = self
                .unserved_reported
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(language.language.clone());
            if first {
                self.report(Level::Warning, Kind::Unserved, None, language.explain());
            }
        }
    }

    /// A candidate failed and the ladder goes on to `next`. The last
    /// candidate's failure is not reported here: it is part of the launch's
    /// own failure, which names every candidate.
    pub(super) fn report_candidate_failed(&self, attempt: &Attempt, next: &str) {
        self.report(
            Level::Warning,
            Kind::CandidateFailed,
            Some(&attempt.server),
            format!(
                "failed to start: {}; to fix: {}; trying {next} next",
                attempt.reason, attempt.hint
            ),
        );
    }

    /// The ladder ran out: every candidate failed, and each one's reason
    /// and install hint is in the one event. Attributed to the preferred
    /// candidate, the server the plan proposed.
    pub(super) fn report_exhausted(&self, slot: &ServerSlot) {
        let attempts = slot.failed_attempts();
        let message = match attempts.as_slice() {
            // A stop cleared the attempts under this launch; the slot is
            // no longer its to report on.
            [] => return,
            [only] => format!("failed to start: {}; to fix: {}", only.reason, only.hint),
            several => format!(
                "none of {} candidates started: {}",
                several.len(),
                several
                    .iter()
                    .map(Attempt::explain)
                    .collect::<Vec<_>>()
                    .join(" · ")
            ),
        };
        let preferred = slot.candidate_names().first().map(|name| name.to_string());
        self.report(
            Level::Error,
            Kind::LaunchFailed,
            preferred.as_deref(),
            message,
        );
    }

    /// A launch that started a server ended without publishing it, for a
    /// reason that is not the candidate's (a workspace busy past the
    /// publication bound).
    pub(super) fn report_launch_failed(&self, server: &str, reason: &str) {
        self.report(
            Level::Error,
            Kind::LaunchFailed,
            Some(server),
            reason.to_string(),
        );
    }

    /// A server was published after the candidates above it failed. The
    /// footer of every answer says the same (`Workspace::descriptions`);
    /// this is the one-time notice.
    pub(super) fn report_fallback(&self, slot: &ServerSlot) {
        let fallen_from = slot.failed_attempts();
        if fallen_from.is_empty() {
            return;
        }
        let names: Vec<&str> = fallen_from
            .iter()
            .map(|attempt| attempt.server.as_str())
            .collect();
        self.report(
            Level::Warning,
            Kind::Fallback,
            Some(slot.name()),
            format!(
                "serving in place of {}, which failed to start",
                names.join(", ")
            ),
        );
    }

    /// The idle sweep stopped these servers.
    pub(crate) fn report_released(&self, stopped: &[LspServer]) {
        for server in stopped {
            self.report(
                Level::Info,
                Kind::Released,
                Some(server.name()),
                format!(
                    "released after {}s with no query addressed to it; the next query \
                     starts it again",
                    self.idle_eviction.as_secs()
                ),
            );
        }
    }
}
