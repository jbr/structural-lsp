//! The initialize handshake and readiness wait, per server.

use super::*;

pub(super) fn initialize_params(root_uri: &str, root: &Path, server: Server) -> Value {
    json!({
        "processId": std::process::id(),
        "clientInfo": { "name": "structural-spike", "version": env!("CARGO_PKG_VERSION") },
        "rootUri": root_uri,
        "rootPath": root.to_string_lossy(),
        "workspaceFolders": [{ "uri": root_uri, "name": server.slug() }],
        "trace": "off",
        "initializationOptions": {},
        "capabilities": {
            "workspace": {
                // `workspaceFolders: true` is deliberately NOT advertised.
                // Observed association (not a proven mechanism): with this
                // capability plus the rich textDocument set below, pyright
                // 1.1.403 never logs "Found 2 source files" and never answers
                // textDocument/definition|references|codeAction; dropping this
                // one capability restores answers in ~2.4 s (two reps each).
                // pyright never sent the server-initiated
                // `workspace/workspaceFolders` request in any trace, so this is
                // an observed association, not proof that workspace loading
                // stops or that the rich textDocument block interacts.
                // The transport *does* answer that request correctly (the
                // obligation is implemented); it is simply never exercised.
                // We still send workspaceFolders in the initialize params.
                "configuration": true,
                "symbol": {
                    "symbolKind": {
                        "valueSet": (1..=26).collect::<Vec<_>>()
                    }
                                }
            },
            "textDocument": {
                "synchronization": { "dynamicRegistration": false, "didSave": true },
                "definition": { "linkSupport": false },
                "references": {},
                "documentSymbol": { "hierarchicalDocumentSymbolSupport": true },
                "hover": { "contentFormat": ["markdown", "plaintext"] },
                "codeAction": {
                    "dynamicRegistration": false,
                    "isPreferredSupport": true,
                    "disabledSupport": true,
                    // `dataSupport` + `resolveSupport` are what let a server
                    // defer expensive edits behind `codeAction/resolve`; without
                    // them the menu can be silently thinner than it should be.
                    "dataSupport": true,
                    "resolveSupport": { "properties": ["edit"] },
                    "codeActionLiteralSupport": {
                        "codeActionKind": {
                            "valueSet": ["", "quickfix", "refactor", "refactor.extract", "refactor.inline", "refactor.rewrite", "source", "source.organizeImports"]
                        }
                    }
                }
            },
            "window": { "workDoneProgress": true, "showMessage": {} },
            "general": { "positionEncodings": ["utf-16"] },
            // rust-analyzer sends `experimental/serverStatus` only when the
            // client advertises this.
            "experimental": { "serverStatusNotification": true }
        }
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
