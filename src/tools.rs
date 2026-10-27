use crate::progress::{self, Status};
use crate::session::Session;
use mcplease::types::RequestContext;
use std::time::{Duration, Instant};

pub mod classify;
pub mod hits;
pub mod listing;
pub mod resolution;
#[cfg(test)]
mod tests;

mcplease::tools!(
    Session,
    (Definition, definition, "definition"),
    (References, references, "references"),
    (Rename, rename, "rename")
);

/// An optional string argument as given, or `None` when it is absent or
/// blank, so `""` means the same as leaving the argument out.
pub(crate) fn given(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|value| !value.is_empty())
}

/// How long a call runs before its first progress notification: long enough
/// that a warm query never sends one.
const FIRST_TICK: Duration = Duration::from_secs(1);

/// How often a call still running says so after that.
const TICK: Duration = Duration::from_secs(5);

/// Run a tool body on smol's blocking pool, reporting progress while it runs.
///
/// The bodies are synchronous and wait on language servers for as long as
/// they need (`docs/correctness.md`), which on a cold index is minutes; they
/// must not stall the executor other calls share, and the client should hear
/// that they are alive. So while the body runs, this sends
/// `notifications/progress` — elapsed seconds, and what the body says it is
/// waiting on ([`progress::waiting_on`]) — to a caller that asked for progress.
/// A caller that sent no progress token hears nothing.
///
/// Cancellation drops this future, which stops the heartbeat and the answer;
/// the body itself runs to completion, holding the workspace until it does.
pub(crate) async fn unblock<T: Send + 'static>(
    context: &RequestContext,
    body: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
) -> anyhow::Result<T> {
    let status = Status::default();
    let task = smol::unblock({
        let status = status.clone();
        move || {
            let _installed = progress::install(status);
            body()
        }
    });
    let started = Instant::now();
    let heartbeat = async {
        smol::Timer::after(FIRST_TICK).await;
        loop {
            let elapsed = started.elapsed().as_secs();
            let message = match status.current() {
                Some(waiting) => format!("{waiting} ({elapsed}s)"),
                None => format!("working ({elapsed}s)"),
            };
            if context
                .progress(elapsed as f64, None, Some(&message))
                .is_err()
            {
                // Cancelled: the call is being dropped; say nothing more.
                std::future::pending::<()>().await;
            }
            smol::Timer::after(TICK).await;
        }
    };
    smol::future::or(task, async {
        heartbeat.await;
        unreachable!("the heartbeat never ends")
    })
    .await
}
