//! Timers and teardown for the owner thread.

use super::*;

impl Owner {
    // -- timers --

    /// When an operation admitted at `admitted_at` with this backstop trips:
    /// inactivity is measured from the later of admission and the server's
    /// last message, so a long-idle server gets a fresh budget on a new
    /// operation and any message extends every pending one.
    pub(super) fn deadline_for(&self, admitted_at: Instant, backstop: Duration) -> Instant {
        let since = match self.last_incoming {
            Some(last) if last > admitted_at => last,
            _ => admitted_at,
        };
        since + backstop
    }

    /// When the next liveness probe is due: `probe_after` past the last
    /// server message (or the earliest admission, if the server has not
    /// spoken since), while caller work is pending and no probe is out.
    fn probe_due(&self) -> Option<Instant> {
        if self.probe_outstanding || self.failed.is_some() || self.shutting_down.is_some() {
            return None;
        }
        let earliest_admission = self
            .pending
            .values()
            .filter(|p| p.reply.is_some())
            .map(|p| p.admitted_at)
            .chain(self.waiters.iter().map(|w| w.admitted_at))
            .chain(self.deferred.iter().map(|d| d.admitted_at))
            .chain(self.writes.values().map(|w| w.admitted_at))
            .min()?;
        let since = match self.last_incoming {
            Some(last) if last > earliest_admission => last,
            _ => earliest_admission,
        };
        Some(since + self.settings.probe_after)
    }

    /// Send the liveness probe when it is due. Best effort: an overloaded
    /// writer just means the next wake tries again.
    pub(super) fn probe_if_silent(&mut self) {
        let Some(due) = self.probe_due() else {
            return;
        };
        if Instant::now() < due {
            return;
        }
        let id = RequestId::Integer(self.next_id);
        self.next_id += 1;
        let frame = JsonRpcMessage::Request(JsonRpcRequest::new(id.clone(), PROBE_METHOD, None));
        self.pending.insert(
            id.clone(),
            PendingRequest {
                method: PROBE_METHOD.to_string(),
                reply: None,
                admitted_at: Instant::now(),
                backstop: self.settings.backstop,
                guard: None,
            },
        );
        match self.enqueue(&frame, None, None) {
            Ok(()) => self.probe_outstanding = true,
            Err(_) => {
                self.pending.remove(&id);
            }
        }
    }

    pub(super) fn next_deadline(&self) -> Option<Instant> {
        let pending = self
            .pending
            .values()
            .map(|p| self.deadline_for(p.admitted_at, p.backstop));
        let waiters = self
            .waiters
            .iter()
            .map(|w| self.deadline_for(w.admitted_at, w.backstop));
        let deferred = self
            .deferred
            .iter()
            .map(|d| self.deadline_for(d.admitted_at, d.backstop));
        let writes = self
            .writes
            .values()
            .map(|w| self.deadline_for(w.admitted_at, w.backstop));
        pending
            .chain(waiters)
            .chain(deferred)
            .chain(writes)
            .chain(self.shutting_down)
            .chain(self.probe_due())
            .min()
    }

    pub(super) fn expire_overdue(&mut self) {
        if self.failed.is_some() {
            return;
        }
        let now = Instant::now();
        let overdue: Vec<RequestId> = self
            .pending
            .iter()
            .filter(|(_, p)| now >= self.deadline_for(p.admitted_at, p.backstop))
            .map(|(id, _)| id.clone())
            .collect();
        for id in overdue {
            let pending = self.pending.remove(&id).expect("collected above");
            if pending.method == PROBE_METHOD {
                self.probe_outstanding = false;
                continue;
            }
            let error =
                self.inactivity_error(&pending.method, pending.admitted_at, pending.backstop);
            if let Some(reply) = pending.reply {
                let _ = reply.send(Err(error));
            }
        }
        let waiters = std::mem::take(&mut self.waiters);
        for waiter in waiters {
            if now >= self.deadline_for(waiter.admitted_at, waiter.backstop) {
                let error =
                    self.inactivity_error("readiness wait", waiter.admitted_at, waiter.backstop);
                let _ = waiter.reply.send(Err(error));
            } else {
                self.waiters.push(waiter);
            }
        }
        let deferred = std::mem::take(&mut self.deferred);
        for held in deferred {
            if now >= self.deadline_for(held.admitted_at, held.backstop) {
                let error = self.inactivity_error(
                    &format!("{} (held for readiness)", held.method),
                    held.admitted_at,
                    held.backstop,
                );
                let _ = held.reply.send(Err(error));
            } else {
                self.deferred.push(held);
            }
        }
        let overdue: Vec<u64> = self
            .writes
            .iter()
            .filter(|(_, w)| now >= self.deadline_for(w.admitted_at, w.backstop))
            .map(|(id, _)| *id)
            .collect();
        for id in overdue {
            let write = self.writes.remove(&id).expect("collected above");
            let error = self.inactivity_error(
                &format!("{} (a notification write)", write.method),
                write.admitted_at,
                write.backstop,
            );
            let _ = write.ack.send(Err(error));
        }
    }

    pub(super) fn inactivity_error(
        &self,
        what: &str,
        admitted_at: Instant,
        backstop: Duration,
    ) -> anyhow::Error {
        let since = match self.last_incoming {
            Some(last) if last > admitted_at => last,
            _ => admitted_at,
        };
        let probed = if self.probe_outstanding {
            "; a liveness probe went unanswered"
        } else {
            ""
        };
        anyhow!(
            "no message from {} for {:.1?} (inactivity backstop {:?}) while {what} was pending, admitted {:.1?} ago; last message: {}{probed}",
            self.label,
            since.elapsed(),
            backstop,
            admitted_at.elapsed(),
            self.last_summary.as_deref().unwrap_or("none yet")
        )
    }

    // -- lifecycle --

    /// The graceful path: ask the server to shut down, bounded by the grace
    /// window. Idempotent; a no-op once the connection has failed.
    pub(super) fn begin_shutdown(&mut self) {
        if self.shutting_down.is_some() || self.failed.is_some() {
            return;
        }
        let grace = self.settings.shutdown_grace;
        self.shutting_down = Some(Instant::now() + grace);
        let id = RequestId::Integer(self.next_id);
        self.next_id += 1;
        self.pending.insert(
            id.clone(),
            PendingRequest {
                method: "shutdown".to_string(),
                reply: None,
                admitted_at: Instant::now(),
                backstop: grace,
                guard: None,
            },
        );
        let frame = JsonRpcMessage::Request(JsonRpcRequest::new(id.clone(), "shutdown", None));
        if self.enqueue(&frame, None, None).is_err() {
            // Best effort only; the kill at the end of the window is the
            // guarantee.
            self.pending.remove(&id);
        }
    }

    /// A fatal condition: fail every waiter, kill and reap the child so a
    /// writer blocked on its stdin unblocks, and let the loop exit.
    pub(super) fn fail(&mut self, reason: String) {
        if self.failed.is_some() {
            return;
        }
        self.failed = Some(reason.clone());
        self.fail_all(&format!("connection to {} failed: {reason}", self.label));
        self.kill_and_reap();
    }

    pub(super) fn fail_all(&mut self, reason: &str) {
        for (_, pending) in self.pending.drain() {
            if let Some(reply) = pending.reply {
                let _ = reply.send(Err(anyhow!("{reason} ({} was pending)", pending.method)));
            }
        }
        for waiter in self.waiters.drain(..) {
            let _ = waiter
                .reply
                .send(Err(anyhow!("{reason} (readiness wait was pending)")));
        }
        for held in self.deferred.drain(..) {
            let _ = held.reply.send(Err(anyhow!(
                "{reason} ({} was held for readiness)",
                held.method
            )));
        }
        for (_, write) in self.writes.drain() {
            let _ = write.ack.send(Err(anyhow!(
                "{reason} ({} was being written)",
                write.method
            )));
        }
    }

    pub(super) fn kill_and_reap(&mut self) {
        // Kill, then close the pipes by reaping: a writer blocked on stdin
        // gets EPIPE, a reader gets EOF.
        self.writer_terminating.store(true, Ordering::SeqCst);
        self.child.kill_and_reap();
    }

    pub(super) fn teardown(&mut self) {
        if self.failed.is_none() {
            self.fail_all(&format!("connection to {} shut down", self.label));
        }
        self.kill_and_reap();
        // The writer exits when its queue closes (it never blocks on the
        // inbox); the reader when its pipe ends — and its final sends need
        // room in the inbox, so drain while waiting for it.
        drop(self.writer_tx.take());
        if let Some(writer) = self.writer.take() {
            bounded_join(writer, JOIN_GUARD);
        }
        if let Some(reader) = self.reader.take() {
            let deadline = Instant::now() + JOIN_GUARD;
            while !reader.is_finished() && Instant::now() < deadline {
                while self.inbox.try_recv().is_ok() {}
                thread::sleep(Duration::from_millis(10));
            }
            if reader.is_finished() {
                let _ = reader.join();
            }
        }
    }
}
