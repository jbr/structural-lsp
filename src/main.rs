//! The `structural` binary: an MCP server over stdio (`structural serve`) or a
//! tool run directly from argv (`structural show choose`). mcplease owns the
//! argv parsing for both.
//!
//! The workspace root is the current directory, or `STRUCTURAL_ROOT` when
//! set: the MCP front end's default workspace context, acquired through the
//! same host an embedded consumer uses. Every configured language server
//! starts before either path runs, so indexing begins immediately.
//!
//! Shutdown is best-effort graceful, escalated by the caller (`docs/shutdown.md`):
//! on Unix, the first SIGINT/SIGTERM/SIGHUP requests a soft shutdown — new tool
//! calls are refused, admitted ones may finish — and a second signal forces
//! server teardown, interrupting even a call that is stuck waiting on a server
//! that answers liveness probes but never finishes. Either way the process
//! reaps every child it owns before exiting. Without signals — an MCP harness
//! closing our stdin — the same `host.shutdown()` runs on the way out.

use anyhow::Context;
use std::path::PathBuf;
use std::sync::Arc;
use structural::host::{AgentId, Host};
use structural::tools;

fn main() -> anyhow::Result<()> {
    let host = Arc::new(Host::new());
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
        let mut session = host.agent_started(AgentId::mcp(), &root);
        let instructions = "\
show resolves a symbol to its definition's decorated source (or another \
`view` of it: outline, implementations, calls, type); uses lists the \
references to it, one source line per hit with its enclosing item, tests set \
apart heuristically; action previews and applies a rename, following \
identity across files. Target by name (`symbol`, bare or qualified by its \
container: `Config::new`) or by quoting source text (`anchor` from the file \
`path`, `focus` naming the token), which reaches locals and call sites. \
Candidates come from the language servers' fuzzy symbol index and are not \
proven exhaustive; only exact names resolve, and several distinct definitions \
are listed rather than picked. References follow symbol identity, not text. \
Every answer is current: owned files are re-synced from disk and the servers \
are waited for before each query. The workspace root is fixed at server \
start; `path` only scopes inside it.";

        let config =
            mcplease::ServerConfig::new(mcplease::server_info!()).with_instructions(instructions);
        if std::env::args().nth(1).as_deref() == Some("serve") {
            #[cfg(unix)]
            let reader = std::io::BufReader::new(input);
            #[cfg(not(unix))]
            let reader = std::io::BufReader::new(EofWatcher {
                inner: std::io::stdin(),
                host: Arc::clone(&host),
            });
            mcplease::serve_with::<tools::Tools, _, _, _>(
                reader,
                std::io::stdout(),
                &mut session,
                &config,
            )
        } else {
            // Preserve clap dispatch, help, one-shot tool output and errors.
            mcplease::run::<tools::Tools, _>(&mut session, config)
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
                    // serve_with still drains responses and returns naturally.
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
