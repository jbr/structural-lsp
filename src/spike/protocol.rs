//! The initialize handshake and readiness wait, per server.

use super::*;

/// The adapter's capabilities and options, with the spike's own client name.
pub(super) fn initialize_params(
    root_uri: &str,
    root: &Path,
    server: Server,
    adapter: &dyn Adapter,
) -> Value {
    json!({
        "processId": std::process::id(),
        "clientInfo": { "name": "structural-spike", "version": env!("CARGO_PKG_VERSION") },
        "rootUri": root_uri,
        "rootPath": root.to_string_lossy(),
        "workspaceFolders": [{ "uri": root_uri, "name": server.slug() }],
        "trace": "off",
        "initializationOptions": adapter.initialization_options(),
        "capabilities": adapter.client_capabilities(),
    })
}

pub(super) fn wait_for_ready(
    handle: &ServerHandle,
    server: Server,
    options: &Options,
) -> Result<Readiness> {
    let started = Instant::now();
    match server {
        Server::Rust => {
            // No open progress token, and the latest `serverStatus` says
            // quiescent. Bounded only by inactivity: a server still reporting
            // progress is waited for.
            handle
                .wait_ready(options.quiesce)
                .context("waiting for rust-analyzer to have no outstanding work")?;
            let evidence = handle
                .capture_events()?
                .into_iter()
                .filter(|event| {
                    event.get("method").and_then(Value::as_str) == Some("experimental/serverStatus")
                })
                .collect();
            Ok(Readiness {
                strategy: "no open progress token and rust-analyzer `experimental/serverStatus` quiescent=true".into(),
                elapsed: started.elapsed(),
                quiescent: true,
                evidence,
            })
        }
        Server::Python => {
            // pyright emits no quiescence signal; standard progress is the
            // only readiness it reports.
            handle
                .wait_ready(options.pyright_wait)
                .context("waiting for pyright to have no outstanding work")?;
            Ok(Readiness {
                strategy: "pyright has no readiness signal beyond standard progress; waited until no progress token was open".into(),
                elapsed: started.elapsed(),
                quiescent: false,
                evidence: vec![json!({
                    "note": "pyright advertises no readiness notification; readiness here is standard progress only, not observed quiescence",
                })],
            })
        }
    }
}
