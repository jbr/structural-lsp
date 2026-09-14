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
    client: &mut Client,
    server: Server,
    options: &Options,
) -> Result<Readiness> {
    let started = Instant::now();
    let mut evidence = Vec::new();

    match server {
        Server::Rust => {
            let deadline = started + options.quiesce;
            let mut last_len = 0usize;
            loop {
                client.pump(Duration::from_millis(250))?;
                for event in &client.events[last_len..] {
                    if event.get("method").and_then(Value::as_str)
                        == Some("experimental/serverStatus")
                    {
                        evidence.push(event.clone());
                    }
                }
                last_len = client.events.len();

                let quiescent = evidence.iter().rev().find_map(|event| {
                    event
                        .get("params")
                        .and_then(|p| p.get("quiescent"))
                        .and_then(Value::as_bool)
                });
                if quiescent == Some(true) {
                    return Ok(Readiness {
                        strategy: "rust-analyzer `experimental/serverStatus` quiescent=true".into(),
                        elapsed: started.elapsed(),
                        quiescent: true,
                        evidence,
                    });
                }
                if Instant::now() >= deadline {
                    break;
                }
            }
            Ok(Readiness {
                strategy: format!(
                    "timed out after {:.0}s without serverStatus quiescent=true; queries proceed and may be incomplete",
                    options.quiesce.as_secs_f32()
                ),
                elapsed: started.elapsed(),
                quiescent: false,
                evidence,
            })
        }
        Server::Python => {
            // pyright emits no quiescence signal at all (design.md's readiness
            // section only covers rust-analyzer). Bounded wait, then proceed,
            // and say so rather than infer readiness.
            client.pump(options.pyright_wait)?;
            evidence.push(json!({
                "note": "pyright advertises no readiness notification; this is a bounded wait, not observed quiescence",
            }));
            Ok(Readiness {
                strategy: format!(
                    "pyright has no readiness signal; bounded {:.0}s wait, then first query",
                    options.pyright_wait.as_secs_f32()
                ),
                elapsed: started.elapsed(),
                quiescent: false,
                evidence,
            })
        }
    }
}
