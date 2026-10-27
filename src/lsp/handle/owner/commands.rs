//! Callers' commands: draining the command queue the run loop wakes for,
//! and admitting each one against the owner's state (docs/transport.md).

use super::*;

impl Owner {
    /// Admit what callers have queued, at most one queue's worth per wake so
    /// that commands arriving during the drain cannot hold off the server's
    /// frames. Each caller blocks on its own command, so the queue holds at
    /// most one per calling thread and a drain ends.
    pub(super) fn drain_commands(&mut self) {
        for _ in 0..self.settings.command_cap {
            match self.commands.try_recv() {
                Ok(command) => self.admit(command),
                Err(_) => return,
            }
        }
    }

    pub(super) fn admit(&mut self, command: Command) {
        match command {
            Command::Request {
                method,
                params,
                reply,
                admitted_at,
                guarded,
            } => self.admit_request(method, params, reply, admitted_at, guarded),
            Command::Notify {
                method,
                params,
                ack,
                admitted_at,
            } => {
                if let Some(reason) = self.refusal() {
                    let _ = ack.send(Err(anyhow!("{reason}")));
                    return;
                }
                if self.operations() >= self.settings.pending_cap {
                    let _ = ack.send(Err(anyhow!(
                        "{}: {} operations already pending (cap); {method} not admitted",
                        self.label,
                        self.settings.pending_cap
                    )));
                    return;
                }
                let frame = JsonRpcMessage::Notification(JsonRpcNotification::new(
                    method.as_str(),
                    Some(params),
                ));
                // Tracked so the backstop can fail a caller whose write is
                // stuck behind a server that stopped reading.
                let id = self.next_write;
                self.next_write += 1;
                self.writes.insert(
                    id,
                    PendingWrite {
                        method: method.clone(),
                        ack: ack.clone(),
                        admitted_at,
                        backstop: self.settings.backstop,
                    },
                );
                if let Err(reason) = self.enqueue(&frame, Some(ack.clone()), Some(id)) {
                    self.writes.remove(&id);
                    let _ = ack.send(Err(anyhow!("{}: {method} not sent: {reason}", self.label)));
                }
            }
            Command::WaitReady {
                backstop,
                reply,
                admitted_at,
            } => {
                if let Some(reason) = self.refusal() {
                    let _ = reply.send(Err(anyhow!("{reason}")));
                    return;
                }
                if self.ready() {
                    let _ = reply.send(Ok(()));
                    return;
                }
                if self.operations() >= self.settings.pending_cap {
                    let _ = reply.send(Err(anyhow!(
                        "{}: {} operations already pending (cap); readiness wait not admitted",
                        self.label,
                        self.settings.pending_cap
                    )));
                    return;
                }
                self.waiters.push(Waiter {
                    reply,
                    admitted_at,
                    backstop,
                });
            }
            Command::Snapshot { reply } => {
                let _ = reply.send(self.snapshot());
            }
            Command::CaptureEvents { reply } => {
                let _ = reply.send(self.events.iter().cloned().collect());
            }
            Command::WatchedFiles { reply } => {
                // A server that never registered is told,
                // unsolicited. One that registered and then
                // withdrew every registration has asked not to be
                // told.
                let wants =
                    !matches!(&self.watched_files, Some(registrations) if registrations.is_empty());
                let _ = reply.send(wants);
            }
            Command::Shutdown => self.begin_shutdown(),
        }
    }

    pub(super) fn refusal(&self) -> Option<String> {
        if let Some(reason) = &self.failed {
            return Some(format!("connection to {} failed: {reason}", self.label));
        }
        if self.shutting_down.is_some() {
            return Some(format!("connection to {} is shutting down", self.label));
        }
        None
    }

    pub(super) fn operations(&self) -> usize {
        self.pending.len() + self.waiters.len() + self.deferred.len() + self.writes.len()
    }

    pub(super) fn ready(&self) -> bool {
        self.progress.is_idle() && self.policy.is_ready()
    }

    pub(super) fn snapshot(&self) -> Snapshot {
        Snapshot {
            label: self.label.clone(),
            ready: self.ready(),
            open_progress_tokens: self.progress.len(),
            pending_operations: self.operations(),
            generation: self.generation,
            reasked: self.reasked,
            last_message: self.last_summary.clone(),
            since_last_message: self.last_incoming.map(|at| at.elapsed()),
            failure: self.failed.clone(),
        }
    }

    pub(super) fn complete_ready_waiters(&mut self) {
        if self.waiters.is_empty() || !self.ready() {
            return;
        }
        for waiter in self.waiters.drain(..) {
            let _ = waiter.reply.send(Ok(()));
        }
    }
}
