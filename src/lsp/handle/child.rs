//! Exclusive process ownership, including cleanup before the owner thread starts.

use super::*;

/// No code outside this guard may reap the child. Keeping the leader unreaped
/// pins its PID (and therefore our PGID) until after the last group signal.
/// Stop capabilities carry flags, never numeric process IDs.
pub(super) struct OwnedChild(Option<Child>);

impl OwnedChild {
    pub(super) fn spawn(command: &str, args: &[String]) -> std::io::Result<Self> {
        let mut command = std::process::Command::new(command);
        command
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());
        // The child becomes its own process-group leader before exec. Never
        // signal the parent's group. Descendants that detach are not covered.
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        command.spawn().map(|child| Self(Some(child)))
    }

    pub(super) fn pipes(&mut self) -> Result<(ChildStdin, ChildStdout)> {
        let child = self.0.as_mut().expect("child not reaped before startup");
        let stdin = child.stdin.take().context("child stdin was not piped")?;
        let stdout = child.stdout.take().context("child stdout was not piped")?;
        Ok((stdin, stdout))
    }

    pub(super) fn kill_and_reap(&mut self) {
        let Some(mut child) = self.0.take() else {
            return;
        };
        #[cfg(unix)]
        {
            // SAFETY: this is the positive PID of our unreaped group leader,
            // created with process_group(0). We never wait/try_wait before
            // this signal, even if stdout closed or the leader already died.
            // Once wait releases the PID, self no longer contains the child.
            let pid = i32::try_from(child.id()).expect("Unix child PID fits pid_t");
            assert!(pid > 0);
            unsafe { libc::kill(-pid, libc::SIGKILL) };
        }
        // Direct-child fallback also covers non-Unix targets. Child::kill
        // alone does not reclaim descendants retaining inherited pipes.
        let _ = child.kill();
        let _ = child.wait();
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        self.kill_and_reap();
    }
}
