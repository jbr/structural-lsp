//! The `structural-lsp` binary: an MCP server over stdio
//! (`structural-lsp serve`) or a tool run directly from argv
//! (`structural-lsp definition choose`), parsed by mcplease's [`Cli`] and
//! run on smol. Every tool call is spawned, so a call waiting on a
//! cold index never holds up another, nor `ping`.
//!
//! The workspace root is the current directory, or `STRUCTURAL_ROOT` when
//! set: the MCP front end's default workspace context, acquired through the
//! same host an embedded consumer uses. A call naming another `workspace`
//! binds the same agent through that host for the call, so the process cwd
//! and the default never change (`docs/ownership.md`). Every configured
//! language server is started before either path runs, so indexing begins
//! immediately — and none of them is waited for here, so the MCP handshake
//! is answered while the indexes are still warming (`docs/ownership.md`).
//! The first tool call waits for them.
//!
//! Shutdown is best-effort graceful, escalated by the caller (`docs/shutdown.md`):
//! on Unix, the first SIGINT/SIGTERM/SIGHUP requests a soft shutdown — new tool
//! calls are refused, admitted ones may finish — and a second signal forces
//! server teardown, interrupting even a call that is stuck waiting on a server
//! that answers liveness probes but never finishes. Either way the process
//! reaps every child it owns before exiting. Without signals — an MCP harness
//! closing our stdin — the same `host.shutdown()` runs on the way out.
//!
//! What the host reports for the operator — a server that would not start, a
//! fallback, a language nothing serves — goes to stderr, one line per event,
//! and to the MCP client as logging (`notifications/message`), which is why
//! the server declares the `logging` capability. The connection's notifier
//! is attached at the first tool call admitted once the client has completed
//! its handshake; events from the eager warm before then are held and sent
//! first (`docs/operator.md`).

use anyhow::Context;
use mcplease::{Cli, clap::Parser, traits::Dispatch, types::RequestContext};
use smol::{Executor, Unblock};
use std::path::PathBuf;
use std::sync::Arc;
use structural_lsp::host::{AgentId, Host};
use structural_lsp::operator::mcp::Mcp;
use structural_lsp::tools;

fn main() -> anyhow::Result<()> {
    let operator = Arc::new(Mcp::new());
    let host = Arc::new(Host::new().with_operator(Arc::clone(&operator) as _));
    // Register before root resolution or eager language-server startup.
    #[cfg(unix)]
    let (signals, input) = signals::install(Arc::clone(&host))?;
    let result = (|| {
        let root = match std::env::var_os("STRUCTURAL_ROOT") {
            Some(root) => PathBuf::from(root),
            None => std::env::current_dir().context("resolving the workspace root")?,
        };
        let root = std::fs::canonicalize(&root)
            .with_context(|| format!("workspace root `{}` does not exist", root.display()))?;
        // Bound so that a call naming another `workspace` binds the same
        // agent through this host, sharing that root's servers
        // (`docs/ownership.md`).
        let session = host.bind(AgentId::mcp(), &root);
        let instructions = "\
You have LSP-backed code intelligence: `definition` (go to definition and \
other IDE views), `references` (find all references), `rename` (rename a \
symbol everywhere). Prefer these over `read` when you need one item's body, \
and over `grep` when a name may be re-exported, shadowed, or spread across \
files: they follow symbol identity, not text. Start with \
`{\"symbol\": \"Config::new\"}`; qualify (`Config::new`) when a bare name \
is ambiguous. The same target takes `view` on `definition`: `outline`, \
`implementations`, `incoming-calls`, `outgoing-calls`, `type` (hover), \
plus per-server `expanded` and `related-tests`. To reach a local, call \
site, or expression, quote source text (`anchor` in the file `path`, \
`focus` naming the token) instead of a line number: your snapshot of a \
file may be stale. `rename` previews with a `digest`, then `apply` needs \
that digest back. `path` scopes where the symbol is looked up, not where \
results are listed. Edits on disk, yours or anyone's, are synced before \
each query. `path` \
scopes inside the workspace root; `workspace` names another project's \
directory for one call, and `language` (`rust`, `python`, `ts`) asks only \
that language's servers.";

        let config = mcplease::ServerConfig::new(mcplease::server_info!())
            .with_instructions(instructions)
            .with_logging();
        // `try_parse`, so help and usage errors return here and the host is
        // still shut down on the way out.
        let command = match Cli::<tools::Tools>::try_parse() {
            Ok(command) => command,
            Err(error) => {
                let _ = error.print();
                return Ok(());
            }
        };
        match command {
            Cli::Tool(tool) => {
                let output = smol::block_on(tool.call_to_text(session, RequestContext::default()))?;
                println!("{output}");
                Ok(())
            }
            Cli::Serve => {
                #[cfg(unix)]
                let reader = input;
                #[cfg(not(unix))]
                let reader = EofWatcher {
                    inner: std::io::stdin(),
                    host: Arc::clone(&host),
                };
                let executor = Executor::new();
                smol::block_on(executor.run(async {
                    let stdin = Unblock::new(reader);
                    let stdout = Unblock::new(std::io::stdout());
                    let mut calls =
                        mcplease::calls::<tools::Tools, _>(stdin, stdout, session, &config);
                    let notifier = calls.notifier();
                    while let Some(call) = calls.next().await {
                        // Each admitted call looks once whether the
                        // handshake has completed; the first look that
                        // finds it so sends what was held. Attaching again
                        // is a no-op.
                        if notifier.is_open() {
                            operator.attach(Arc::new(notifier.clone()));
                        }
                        executor.spawn(call?).detach();
                    }
                    Ok(())
                }))
            }
        }
    })();
    if result.is_err() {
        host.force_shutdown();
    }
    host.shutdown();
    #[cfg(unix)]
    signals.finish();
    result
}

#[cfg(not(unix))]
struct EofWatcher<R> {
    inner: R,
    host: Arc<Host>,
}

#[cfg(not(unix))]
impl<R: std::io::Read> std::io::Read for EofWatcher<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let result = self.inner.read(buf);
        if !buf.is_empty() && !matches!(result, Ok(n) if n > 0) {
            self.host.force_shutdown();
        }
        result
    }
}

#[cfg(unix)]
mod signals {
    use super::*;
    use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
    use signal_hook::iterator::{Handle, Signals};
    use std::io::{self, Read};
    use std::os::fd::AsRawFd;
    use std::os::unix::net::UnixStream;
    use std::sync::mpsc;
    use std::thread::{self, JoinHandle};

    pub(super) struct Watcher {
        handle: Handle,
        watcher: JoinHandle<()>,
        cleanup: JoinHandle<()>,
    }

    impl Watcher {
        pub(super) fn finish(self) {
            // Wakes the iterator even when no signal ever arrived (EOF).
            self.handle.close();
            let _ = self.watcher.join();
            let _ = self.cleanup.join();
        }
    }

    pub(super) fn install(host: Arc<Host>) -> anyhow::Result<(Watcher, Input)> {
        // Maintained handler owns its wake descriptors and registration.
        // Rust's socket pair also sets CLOEXEC, unlike the former raw pipe.
        let mut signals = Signals::new([SIGINT, SIGTERM, SIGHUP])?;
        let handle = signals.handle();
        let (cancel, wake) = UnixStream::pair()?;
        let input = Input {
            cancel,
            host: Arc::clone(&host),
        };
        let (start, requested) = mpsc::sync_channel(1);
        let cleanup_host = Arc::clone(&host);
        let cleanup = thread::Builder::new()
            .name("structural-cleanup".into())
            .spawn(move || {
                if requested.recv().is_ok() {
                    cleanup_host.wait_shutdown();
                    // End input only after admitted local work has finished.
                    // The serve loop still drains responses and returns naturally.
                    let _ = wake.shutdown(std::net::Shutdown::Both);
                }
            })?;
        let watcher = match thread::Builder::new()
            .name("structural-signals".into())
            .spawn(move || {
                let mut first = true;
                for _ in signals.forever() {
                    if first {
                        first = false;
                        host.request_shutdown();
                        let _ = start.try_send(());
                        eprintln!("structural: graceful shutdown requested");
                    } else {
                        host.force_shutdown();
                        eprintln!("structural: forced shutdown requested");
                    }
                }
            }) {
            Ok(watcher) => watcher,
            Err(error) => {
                handle.close();
                let _ = cleanup.join();
                return Err(error.into());
            }
        };
        Ok((
            Watcher {
                handle,
                watcher,
                cleanup,
            },
            input,
        ))
    }

    /// Wait on stdin and a cancellation descriptor together. Cancellation is
    /// synthetic EOF, not transport loss: it must not turn soft into force.
    pub(super) struct Input {
        cancel: UnixStream,
        host: Arc<Host>,
    }

    impl Read for Input {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if buf.is_empty() {
                return Ok(0);
            }
            loop {
                let mut fds = [
                    libc::pollfd {
                        fd: libc::STDIN_FILENO,
                        events: libc::POLLIN,
                        revents: 0,
                    },
                    libc::pollfd {
                        fd: self.cancel.as_raw_fd(),
                        events: libc::POLLIN,
                        revents: 0,
                    },
                ];
                // SAFETY: both descriptors remain owned for this call, and
                // poll only writes within the two initialized pollfd entries.
                let ready = unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) };
                if ready < 0 {
                    let error = io::Error::last_os_error();
                    if error.kind() == io::ErrorKind::Interrupted {
                        continue;
                    }
                    self.host.force_shutdown();
                    return Err(error);
                }
                if fds[1].revents != 0 {
                    return Ok(0);
                }
                if fds[0].revents == 0 {
                    continue;
                }
                // SAFETY: buf is writable for its length. This is the only
                // stdin reader in serve mode; no stdio buffering competes.
                let n =
                    unsafe { libc::read(libc::STDIN_FILENO, buf.as_mut_ptr().cast(), buf.len()) };
                if n < 0 {
                    let error = io::Error::last_os_error();
                    if error.kind() == io::ErrorKind::Interrupted {
                        continue;
                    }
                    self.host.force_shutdown();
                    return Err(error);
                }
                if n == 0 {
                    self.host.force_shutdown();
                }
                return Ok(n as usize);
            }
        }
    }
}
