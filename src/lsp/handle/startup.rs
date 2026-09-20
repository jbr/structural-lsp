//! Process and worker startup. The owned child guard also covers failed thread creation.
use super::*;

pub(super) fn spawn(
    command: &str,
    args: &[String],
    settings: Settings,
    policy: Box<dyn ReadinessPolicy>,
    mut spawn_thread: impl FnMut(&str, Box<dyn FnOnce() + Send>) -> std::io::Result<JoinHandle<()>>,
) -> Result<ServerHandle> {
    let label = std::iter::once(command)
        .chain(args.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join(" ");
    let mut child =
        OwnedChild::spawn(command, args).with_context(|| format!("spawning `{label}`"))?;
    let (stdin, stdout) = child.pipes()?;

    let (inbox_tx, inbox_rx) = mpsc::sync_channel::<OwnerMsg>(settings.inbound_cap);
    let (writer_tx, writer_rx) = mpsc::sync_channel::<WriterJob>(settings.writer_cap);
    let terminate = Arc::new(AtomicBool::new(false));
    let force = Arc::new(AtomicBool::new(false));
    let writer_terminating = Arc::new(AtomicBool::new(false));

    let spawned = (|| -> Result<(JoinHandle<()>, JoinHandle<()>)> {
        let reader = spawn_thread(
            "lsp-reader",
            Box::new({
                let inbox = inbox_tx.clone();
                move || reader_loop(BufReader::new(stdout), inbox)
            }),
        )
        .context("spawning reader thread")?;
        let writer = spawn_thread(
            "lsp-writer",
            Box::new({
                let inbox = inbox_tx.clone();
                let terminating = Arc::clone(&writer_terminating);
                move || writer_loop(stdin, writer_rx, terminating, inbox)
            }),
        )
        .context("spawning writer thread")?;
        Ok((reader, writer))
    })();
    let (reader, writer) = match spawned {
        Ok(threads) => threads,
        Err(error) => {
            child.kill_and_reap();
            return Err(error);
        }
    };

    let inbound_cap = settings.inbound_cap;
    let progress = ProgressTokens::new(settings.progress_token_cap, settings.progress_create_is_work);
    let owner = Owner {
        label: label.clone(),
        settings,
        policy,
        child,
        inbox: inbox_rx,
        writer_tx: Some(writer_tx),
        writer: Some(writer),
        reader: Some(reader),
        terminate: Arc::clone(&terminate),
        force: Arc::clone(&force),
        writer_terminating,
        next_id: 1,
        pending: HashMap::new(),
        waiters: Vec::new(),
        writes: HashMap::new(),
        next_write: 0,
        progress,
        events: VecDeque::new(),
        last_incoming: None,
        last_summary: None,
        workspace_folders: Value::Null,
        failed: None,
        shutting_down: None,
        shutdown_acknowledged: false,
        probe_outstanding: false,
        debug: wire_debug_enabled(),
    };
    let owner = spawn_thread("lsp-owner", Box::new(move || owner.run()))
        .context("spawning owner thread")?;

    Ok(ServerHandle {
        label,
        inbound_cap,
        inbox: inbox_tx,
        terminate,
        force,
        owner: Some(owner),
    })
}
