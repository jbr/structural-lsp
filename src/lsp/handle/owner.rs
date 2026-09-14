//! The owner thread: every piece of protocol state, and the only thread
//! that touches it.

use super::*;

mod lifecycle;

pub(super) struct Owner {
    pub(super) label: String,
    pub(super) settings: Settings,
    pub(super) policy: Box<dyn ReadinessPolicy>,
    pub(super) child: Child,
    pub(super) inbox: Receiver<OwnerMsg>,
    pub(super) writer_tx: Option<SyncSender<WriterJob>>,
    pub(super) writer: Option<JoinHandle<()>>,
    pub(super) reader: Option<JoinHandle<()>>,
    pub(super) terminate: Arc<AtomicBool>,
    pub(super) writer_terminating: Arc<AtomicBool>,
    pub(super) next_id: i64,
    pub(super) pending: HashMap<RequestId, PendingRequest>,
    pub(super) waiters: Vec<Waiter>,
    pub(super) writes: HashMap<u64, PendingWrite>,
    pub(super) next_write: u64,
    pub(super) progress: ProgressTokens,
    pub(super) events: VecDeque<Value>,
    pub(super) last_incoming: Option<Instant>,
    pub(super) last_summary: Option<String>,
    pub(super) workspace_folders: Value,
    /// Set by the first fatal condition; the loop exits on the next pass.
    pub(super) failed: Option<String>,
    /// The graceful window's deadline once shutdown has begun.
    pub(super) shutting_down: Option<Instant>,
    pub(super) shutdown_acknowledged: bool,
    pub(super) debug: bool,
}

impl Owner {
    pub(super) fn run(mut self) {
        loop {
            if self.terminate.load(Ordering::SeqCst) {
                self.begin_shutdown();
            }
            if self.done() {
                break;
            }
            let wait = self
                .next_deadline()
                .map(|deadline| deadline.saturating_duration_since(Instant::now()))
                .map_or(CONTROL_TICK, |until| until.min(CONTROL_TICK));
            match self.inbox.recv_timeout(wait) {
                Ok(message) => self.process(message),
                Err(RecvTimeoutError::Timeout) => {}
                // Every sender is gone: the handle, the reader and the
                // writer. Nothing can arrive; tear down.
                Err(RecvTimeoutError::Disconnected) => break,
            }
            // After every wake, message or not, so continuous traffic
            // cannot starve the backstop.
            self.expire_overdue();
        }
        self.teardown();
        self.serve_refusals();
    }

    /// After teardown the owner lingers, without a child or workers, only to
    /// refuse later operations with the real reason and answer snapshots,
    /// until the handle asks it to go. A caller that finds the connection
    /// failed learns *why*, not merely that it is gone.
    pub(super) fn serve_refusals(&mut self) {
        loop {
            if self.terminate.load(Ordering::SeqCst) {
                return;
            }
            match self.inbox.recv_timeout(CONTROL_TICK) {
                Ok(OwnerMsg::Admit(Command::Shutdown)) => return,
                Ok(OwnerMsg::Admit(command)) => self.admit(command),
                Ok(OwnerMsg::FromServer(_))
                | Ok(OwnerMsg::Written(_))
                | Ok(OwnerMsg::WriteFailed(_)) => {}
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    }

    pub(super) fn done(&self) -> bool {
        if self.failed.is_some() {
            return true;
        }
        match self.shutting_down {
            Some(deadline) => self.shutdown_acknowledged || Instant::now() >= deadline,
            None => false,
        }
    }

    pub(super) fn process(&mut self, message: OwnerMsg) {
        match message {
            OwnerMsg::FromServer(incoming) => self.on_incoming(incoming),
            OwnerMsg::Admit(command) => self.admit(command),
            OwnerMsg::Written(id) => {
                self.writes.remove(&id);
            }
            OwnerMsg::WriteFailed(reason) => self.fail(reason),
        }
    }

    // -- incoming --

    pub(super) fn on_incoming(&mut self, incoming: Incoming) {
        if self.debug {
            let summary = match &incoming {
                Incoming::Message(message, _) => summarize(message),
                Incoming::Malformed(error) => format!("MALFORMED {error}"),
                Incoming::Eof => "EOF".to_string(),
            };
            eprintln!("[lsp<] {summary}");
        }
        match incoming {
            Incoming::Eof => self.fail("language server closed stdout".to_string()),
            Incoming::Malformed(error) => {
                self.touch("malformed frame".to_string());
                self.record(json!({ "unparsable": error }));
            }
            Incoming::Message(message, has_result) => {
                self.touch(summarize(&message));
                match *message {
                    JsonRpcMessage::Response(response) => self.on_response(response, has_result),
                    JsonRpcMessage::Notification(notification) => {
                        self.on_notification(notification)
                    }
                    JsonRpcMessage::Request(request) => self.on_server_request(request),
                }
            }
        }
        self.complete_ready_waiters();
    }

    /// Any message resets inactivity for every pending operation.
    pub(super) fn touch(&mut self, summary: String) {
        self.last_incoming = Some(Instant::now());
        self.last_summary = Some(summary);
    }

    pub(super) fn on_response(&mut self, response: JsonRpcResponse, has_result: bool) {
        let Some(id) = response.id.clone() else {
            self.record(serde_json::to_value(&response).unwrap_or(Value::Null));
            return;
        };
        let Some(pending) = self.pending.remove(&id) else {
            // Nobody is waiting: a retired or unknown id. Keep it, do not
            // drop it.
            self.record(serde_json::to_value(&response).unwrap_or(Value::Null));
            return;
        };
        let outcome = jsonrpc::response_result(response, has_result);
        match pending.reply {
            Some(reply) => {
                let _ = reply.send(outcome);
            }
            None if pending.method == "shutdown" => {
                self.shutdown_acknowledged = true;
                let _ = self.enqueue(
                    &JsonRpcMessage::Notification(JsonRpcNotification::new("exit", None)),
                    None,
                    None,
                );
            }
            None => {}
        }
    }

    pub(super) fn on_notification(&mut self, notification: JsonRpcNotification) {
        if let Err(reason) = self
            .progress
            .observe(&notification.method, notification.params.as_ref())
        {
            self.fail(reason);
            return;
        }
        self.policy
            .observe(&notification.method, notification.params.as_ref());
        self.record(serde_json::to_value(&notification).unwrap_or(Value::Null));
    }

    pub(super) fn on_server_request(&mut self, request: JsonRpcRequest) {
        if let Err(reason) = self
            .progress
            .observe(&request.method, request.params.as_ref())
        {
            self.fail(reason);
            return;
        }
        let reply = handlers::server_request_reply(
            &request.method,
            request.params.as_ref(),
            &self.workspace_folders,
        );
        let response = match reply {
            Reply::Success(value) => JsonRpcResponse::success(request.id.clone(), value),
            Reply::Error(code, message) => JsonRpcResponse::error(
                request.id.clone(),
                JsonRpcError {
                    code,
                    message,
                    data: None,
                },
            ),
        };
        // A required frame: the server is waiting on it. Not queueable means
        // the connection cannot be kept coherent.
        if let Err(reason) = self.enqueue(&JsonRpcMessage::Response(response), None, None) {
            self.fail(format!(
                "could not queue the reply to server request {}: {reason}",
                request.method
            ));
            return;
        }
        self.record(serde_json::to_value(&request).unwrap_or(Value::Null));
    }

    // -- commands --

    pub(super) fn admit(&mut self, command: Command) {
        match command {
            Command::Request {
                method,
                params,
                reply,
                admitted_at,
            } => {
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
                    // A server that sees `workspace.workspaceFolders` may
                    // later ask for these.
                    self.workspace_folders = params
                        .get("workspaceFolders")
                        .cloned()
                        .unwrap_or(Value::Null);
                }
                let id = RequestId::Integer(self.next_id);
                self.next_id += 1;
                let frame = JsonRpcMessage::Request(JsonRpcRequest::new(
                    id.clone(),
                    method.as_str(),
                    Some(params),
                ));
                // Registered before the frame can be written: the owner is
                // the only thread that routes responses, so none can be
                // processed between here and the enqueue below.
                self.pending.insert(
                    id.clone(),
                    PendingRequest {
                        method: method.clone(),
                        reply: Some(reply.clone()),
                        admitted_at,
                        backstop: self.settings.backstop,
                    },
                );
                if let Err(reason) = self.enqueue(&frame, None, None) {
                    self.pending.remove(&id);
                    let _ = reply.send(Err(anyhow!("{}: {method} not sent: {reason}", self.label)));
                }
            }
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
        self.pending.len() + self.waiters.len() + self.writes.len()
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

    // -- outgoing --

    /// Serialize and hand a frame to the writer. `Err` is the reason it could
    /// not be queued; nothing was written.
    pub(super) fn enqueue(
        &mut self,
        message: &JsonRpcMessage,
        ack: Option<Sender<Result<()>>>,
        tracked: Option<u64>,
    ) -> Result<(), String> {
        let frame = serde_json::to_vec(message).map_err(|e| format!("serializing frame: {e}"))?;
        let Some(writer) = &self.writer_tx else {
            return Err("writer already released".to_string());
        };
        match writer.try_send(WriterJob {
            frame,
            ack,
            tracked,
        }) {
            Ok(()) => {
                if self.debug {
                    eprintln!("[lsp>] {}", truncated(message));
                }
                // Recorded when handed to the writer, in enqueue order — the
                // order it will reach the wire.
                self.record(serde_json::to_value(message).unwrap_or(Value::Null));
                Ok(())
            }
            Err(TrySendError::Full(_)) => Err(format!(
                "local write queue full ({} frames)",
                self.settings.writer_cap
            )),
            Err(TrySendError::Disconnected(_)) => Err("writer thread gone".to_string()),
        }
    }

    pub(super) fn record(&mut self, value: Value) {
        self.events.push_back(value);
        while self.events.len() > self.settings.event_cap {
            self.events.pop_front();
        }
    }
}

fn summarize(message: &JsonRpcMessage) -> String {
    match message {
        JsonRpcMessage::Request(request) => format!("request {}", request.method),
        JsonRpcMessage::Notification(notification) => {
            format!("notification {}", notification.method)
        }
        JsonRpcMessage::Response(response) => format!("response to id {:?}", response.id),
    }
}

fn truncated(message: &JsonRpcMessage) -> String {
    let text = serde_json::to_string(message).unwrap_or_default();
    if text.chars().count() > 240 {
        format!("{}…", text.chars().take(240).collect::<String>())
    } else {
        text
    }
}
