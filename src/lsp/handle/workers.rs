//! The reader and writer threads.

use super::*;

/// Frames from the server into the owner's inbox. The send blocks when the
/// inbox is full: that is backpressure on the child, by design.
pub(super) fn reader_loop(mut reader: BufReader<ChildStdout>, inbox: SyncSender<OwnerMsg>) {
    loop {
        let incoming = match read_frame(&mut reader) {
            Ok(Some(bytes)) => match jsonrpc::classify_frame(&bytes) {
                Ok((message, has_result)) => Incoming::Message(Box::new(message), has_result),
                // Frame boundary intact: report it and keep reading.
                Err(error) => Incoming::Malformed(format!(
                    "{error}; frame bytes: {}",
                    String::from_utf8_lossy(&bytes)
                )),
            },
            Ok(None) => {
                let _ = inbox.send(OwnerMsg::FromServer(Incoming::Eof));
                return;
            }
            Err(error) => {
                let _ = inbox.send(OwnerMsg::FromServer(Incoming::Malformed(format!(
                    "framing error: {error}"
                ))));
                let _ = inbox.send(OwnerMsg::FromServer(Incoming::Eof));
                return;
            }
        };
        if inbox.send(OwnerMsg::FromServer(incoming)).is_err() {
            return;
        }
    }
}

/// Frames from the owner onto the server's stdin, acknowledging each one
/// with its real outcome. After the first failed write, or once the owner
/// is terminating, every remaining job is acknowledged as not written
/// rather than left unanswered. Exits when the owner drops the queue.
pub(super) fn writer_loop(
    mut stdin: ChildStdin,
    jobs: Receiver<WriterJob>,
    terminating: Arc<AtomicBool>,
    inbox: SyncSender<OwnerMsg>,
) {
    let mut broken: Option<String> = None;
    for job in jobs {
        if let Some(reason) = &broken {
            acknowledge(job.ack, Err(anyhow!("frame not written: {reason}")));
            continue;
        }
        if terminating.load(Ordering::SeqCst) {
            acknowledge(
                job.ack,
                Err(anyhow!(
                    "connection terminated before the frame was written"
                )),
            );
            continue;
        }
        match write_frame(&mut stdin, &job.frame) {
            Ok(()) => acknowledge(job.ack, Ok(())),
            Err(error) => {
                let reason = format!("writing to the server failed: {error}");
                acknowledge(job.ack, Err(anyhow!("{reason}")));
                // Nonblocking: if the inbox is full the owner is draining it
                // and will learn of the death from the reader's EOF instead.
                let _ = inbox.try_send(OwnerMsg::WriteFailed(reason.clone()));
                broken = Some(reason);
            }
        }
    }
}

pub(super) fn acknowledge(ack: Option<Sender<Result<()>>>, outcome: Result<()>) {
    if let Some(ack) = ack {
        let _ = ack.send(outcome);
    }
}
