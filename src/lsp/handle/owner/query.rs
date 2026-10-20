//! Requests on the owner thread, and the guard on semantic ones: written
//! only when the server is ready, answered only by a response no work began
//! before (docs/correctness.md).

use super::*;

impl Owner {
    /// Admit a caller's request: refused when the connection cannot take
    /// it, held when it is semantic and the server has work outstanding,
    /// written otherwise.
    pub(super) fn admit_request(
        &mut self,
        method: String,
        params: Value,
        reply: Sender<Result<Value>>,
        admitted_at: Instant,
        guarded: bool,
    ) {
        if let Some(reason) = self.refusal() {
            let _ = reply.send(Err(anyhow!("{reason}")));
            return;
        }
        if self.operations() >= self.settings.pending_cap {
            let _ = reply.send(Err(anyhow!(
                "{}: {} operations already pending (cap); {method} not admitted",
                self.label,
                self.settings.pending_cap
            )));
            return;
        }
        if method == "initialize" {
            // A server that sees `workspace.workspaceFolders` may later ask
            // for these.
            self.workspace_folders = params
                .get("workspaceFolders")
                .cloned()
                .unwrap_or(Value::Null);
        }
        if guarded && !self.ready() {
            self.deferred.push(Deferred {
                method,
                params,
                reply,
                admitted_at,
                backstop: self.settings.backstop,
            });
            return;
        }
        self.write_request(method, params, reply, admitted_at, guarded);
    }

    /// Register a request and hand it to the writer. A semantic one records
    /// the readiness generation it was written under.
    fn write_request(
        &mut self,
        method: String,
        params: Value,
        reply: Sender<Result<Value>>,
        admitted_at: Instant,
        guarded: bool,
    ) {
        let id = RequestId::Integer(self.next_id);
        self.next_id += 1;
        let guard = guarded.then(|| Guard {
            params: params.clone(),
            generation: self.generation,
        });
        let frame = JsonRpcMessage::Request(JsonRpcRequest::new(
            id.clone(),
            method.as_str(),
            Some(params),
        ));
        // Registered before the frame can be written: the owner is the only
        // thread that routes responses, so none can be processed between
        // here and the enqueue below.
        self.pending.insert(
            id.clone(),
            PendingRequest {
                method: method.clone(),
                reply: Some(reply.clone()),
                admitted_at,
                backstop: self.settings.backstop,
                guard,
            },
        );
        if let Err(reason) = self.enqueue(&frame, None, None) {
            self.pending.remove(&id);
            let _ = reply.send(Err(anyhow!("{}: {method} not sent: {reason}", self.label)));
        }
    }

    /// Count a transition to not-ready caused by the message just applied.
    /// Only a transition counts: a semantic request is written only while
    /// the server is ready, so any work it could straddle begins with one.
    pub(super) fn track_generation(&mut self, was_ready: bool) {
        if was_ready && !self.ready() {
            self.generation += 1;
        }
    }

    /// The pending request a response answers, unless it is a semantic
    /// request answered while work that began after its write is still
    /// outstanding: that answer is set aside, whatever it says, and the
    /// request is held for readiness to be written again.
    ///
    /// Work that began and ended before the answer arrived does not set it
    /// aside. A server sends its own state in order on the one stream, so
    /// that answer was sent by a server that had settled again — and the
    /// work may be the request's own: pyright opens and ends a "Finding
    /// references" token for every `references` it answers, before the
    /// answer, and setting those aside would ask forever
    /// (docs/correctness.md).
    pub(super) fn accept(
        &mut self,
        id: &RequestId,
        pending: PendingRequest,
    ) -> Option<PendingRequest> {
        let written = match &pending.guard {
            Some(guard) if guard.generation != self.generation && !self.ready() => guard.generation,
            _ => return Some(pending),
        };
        let PendingRequest {
            method,
            reply,
            admitted_at,
            backstop,
            guard,
        } = pending;
        let (Some(reply), Some(guard)) = (reply, guard) else {
            return None;
        };
        self.reasked += 1;
        if self.debug {
            eprintln!(
                "[lsp ] set aside the response to {id:?} ({method}): work began after it was written and is outstanding \
                 (generation {written} -> {})",
                self.generation
            );
        }
        self.record(json!({
            "set_aside": serde_json::to_value(id).unwrap_or(Value::Null),
            "method": method,
            "written_generation": written,
            "generation": self.generation,
        }));
        self.deferred.push(Deferred {
            method,
            params: guard.params,
            reply,
            admitted_at,
            backstop,
        });
        None
    }

    /// Write every held semantic request, once the server is ready.
    pub(super) fn release_deferred(&mut self) {
        if self.deferred.is_empty() || !self.ready() || self.refusal().is_some() {
            return;
        }
        for held in std::mem::take(&mut self.deferred) {
            self.write_request(held.method, held.params, held.reply, held.admitted_at, true);
        }
    }
}
