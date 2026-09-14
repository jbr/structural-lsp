//! The throwaway raw-LSP spike (design.md "First step", experiments C and F).
//!
//! It starts one or both language servers, waits as long as it reasonably can
//! for readiness, opens the annotated fixture files, and dumps the raw
//! `workspace/symbol`, `textDocument/definition`, `textDocument/references`,
//! and `textDocument/codeAction` results for every `@decl` and `@expr`
//! annotation. It **never applies an edit and never executes a code-action
//! command**; `workspace/applyEdit` is declined in the transport.
//!
//! Output is deterministic (default `target/spike/<server>/`) so two runs are
//! comparable: `raw/` holds one JSON file per query and `manifest.json` holds
//! the initialize result, readiness evidence, timings, and every notification
//! and server request seen.

use anyhow::{Context, Result, anyhow, bail};
use lsp_types::Position;
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use structural::lsp::adapter::{Adapter, pyright::Pyright, rust_analyzer::RustAnalyzer};
use structural::lsp::handle::{ServerHandle, Settings};

mod annotations;
mod protocol;
mod report;
use annotations::*;
use protocol::*;
use report::*;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Server {
    Rust,
    Python,
}

impl Server {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "rust" => Some(Self::Rust),
            "python" | "pyright" => Some(Self::Python),
            _ => None,
        }
    }

    pub fn slug(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::Python => "python",
        }
    }

    /// The adapter that knows this server: command, capabilities, readiness.
    fn adapter(self) -> Box<dyn Adapter> {
        match self {
            Self::Rust => Box::new(RustAnalyzer),
            Self::Python => Box::new(Pyright),
        }
    }

    fn fixture_dir(self) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("fixtures")
            .join(self.slug())
    }
}

pub struct Options {
    pub servers: Vec<Server>,
    pub out: PathBuf,
    /// Inactivity backstop while waiting for rust-analyzer's readiness: the
    /// wait fails only if the server says nothing for this long.
    pub quiesce: Duration,
    /// The same backstop for pyright, which has no readiness signal beyond
    /// standard progress.
    pub pyright_wait: Duration,
    /// Inactivity backstop for every query.
    pub query_timeout: Duration,
}

pub struct Report {
    pub server: Server,
    pub readiness: Readiness,
    pub total: usize,
    pub ok: usize,
    pub errored: usize,
    pub nonempty: usize,
    pub events: usize,
    pub dir: PathBuf,
}

pub struct Readiness {
    pub strategy: String,
    pub elapsed: Duration,
    pub quiescent: bool,
    pub evidence: Vec<Value>,
}

pub fn run(options: &Options) -> Result<Vec<Report>> {
    fs::create_dir_all(&options.out)
        .with_context(|| format!("creating output dir {}", options.out.display()))?;
    let mut reports = Vec::new();
    for &server in &options.servers {
        match run_one(server, options) {
            Ok(report) => reports.push(report),
            Err(error) => {
                eprintln!("=== {} FAILED: {error:#}", server.slug());
                return Err(error);
            }
        }
    }
    Ok(reports)
}

fn run_one(server: Server, options: &Options) -> Result<Report> {
    let dir = options.out.join(server.slug());
    let raw = dir.join("raw");
    fs::create_dir_all(&raw)?;

    let fixture = server.fixture_dir();
    if !fixture.is_dir() {
        bail!("fixture directory missing: {}", fixture.display());
    }

    let adapter = server.adapter();
    let argv = adapter.command();
    let (command, args) = argv.split_first().expect("an executable");
    let language_id = adapter.languages()[0].id.clone();
    eprintln!(
        "=== {}: spawning `{command} {}`",
        server.slug(),
        args.join(" ")
    );
    let settings = Settings {
        backstop: options.query_timeout,
        // The manifest claims "every notification and server request seen";
        // keep the log effectively uncapped for a spike run.
        event_cap: 1_000_000,
        ..Settings::default()
    };
    let handle = ServerHandle::spawn_with(command, args, settings, adapter.readiness_policy())?;

    let root_uri = url::Url::from_directory_path(&fixture)
        .map_err(|_| anyhow!("invalid fixture path {}", fixture.display()))?
        .to_string();

    let started = Instant::now();
    let initialize = handle.request(
        "initialize",
        initialize_params(&root_uri, &fixture, server, adapter.as_ref()),
    )?;
    // Captured immediately: this is initialize latency, not whole-run time.
    let initialize_elapsed_ms = started.elapsed().as_millis();
    let server_info = initialize.get("serverInfo").cloned().unwrap_or(Value::Null);
    eprintln!(
        "=== {}: initialize in {:.1}s, serverInfo={}",
        server.slug(),
        started.elapsed().as_secs_f32(),
        server_info
    );

    handle.notify("initialized", json!({}))?;

    let readiness = wait_for_ready(&handle, server, options)?;
    eprintln!(
        "=== {}: readiness after {:.1}s (quiescent={}): {}",
        server.slug(),
        readiness.elapsed.as_secs_f32(),
        readiness.quiescent,
        readiness.strategy
    );

    // Open every fixture source file, then wait for the server to have no
    // outstanding work again before querying.
    let sources = collect_sources(&fixture)?;
    for (path, text, uri) in &sources {
        handle.notify(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": language_id,
                    "version": 1,
                    "text": text,
                }
            }),
        )?;
        eprintln!("=== {}: didOpen {}", server.slug(), path.display());
    }
    handle.wait_ready(options.quiesce)?;

    // Readiness observed *before* didOpen is not necessarily query-phase
    // readiness — rust-analyzer's quiescence can precede source loading. Record
    // what the server reports now, after the opens.
    let post_open_status = latest_server_status(&handle)?;
    if post_open_status != Value::Null {
        eprintln!(
            "=== {}: post-open serverStatus: {post_open_status}",
            server.slug()
        );
    }

    let mut total = 0usize;
    let mut ok = 0usize;
    let mut errored = 0usize;
    let mut nonempty = 0usize;
    let mut index = 0usize;
    let mut first_useful_ms: Option<u128> = None;
    let mut actions = ActionTally::default();

    for (path, text, uri) in &sources {
        let annotations = parse_annotations(text);
        for annotation in &annotations {
            let resolved = match resolve_annotation(text, annotation) {
                Some(resolved) => resolved,
                None => {
                    eprintln!(
                        "=== {}: {} could not locate selection {:?} within its region in {}",
                        server.slug(),
                        annotation.query,
                        annotation.selection,
                        path.display()
                    );
                    continue;
                }
            };

            let selection_range = (
                position(text, resolved.selection_start),
                position(text, resolved.selection_end),
            );
            let target = position(text, resolved.target_offset);
            let meta = AnnotationMeta {
                kind: annotation.kind.label(),
                query: annotation.query.clone(),
                selection: annotation.selection.clone(),
                source_line: annotation.source_line,
                selection_range: Some(selection_range),
                target: Some(target),
                target_provenance: resolved.target_provenance,
            };

            let queries: [(&str, Value); 4] = [
                ("workspace/symbol", json!({ "query": annotation.query })),
                (
                    "textDocument/definition",
                    json!({ "textDocument": { "uri": uri }, "position": target }),
                ),
                (
                    "textDocument/references",
                    json!({
                        "textDocument": { "uri": uri },
                        "position": target,
                        "context": { "includeDeclaration": true }
                    }),
                ),
                (
                    "textDocument/codeAction",
                    json!({
                        "textDocument": { "uri": uri },
                        "range": {"start": selection_range.0, "end": selection_range.1},
                        "context": { "diagnostics": [] }
                    }),
                ),
            ];

            for (method, params) in queries {
                index += 1;
                let captured = capture(&handle, method, params.clone());
                let produced_result = captured.error.is_none() && is_nonempty(&captured.result);
                write_raw(&raw, index, &meta, method, &params, &captured)?;
                eprintln!(
                    "=== {}: {} {} [{method}] {} in {}ms ({})",
                    server.slug(),
                    meta.kind,
                    meta.query,
                    if captured.error.is_none() {
                        "ok"
                    } else {
                        "ERR"
                    },
                    captured.elapsed_ms,
                    if produced_result {
                        "non-empty"
                    } else {
                        "empty"
                    }
                );

                total += 1;
                if captured.error.is_none() {
                    ok += 1;
                } else {
                    errored += 1;
                }
                if produced_result {
                    nonempty += 1;
                    first_useful_ms.get_or_insert(started.elapsed().as_millis());
                }

                // A code-action menu may defer its edits behind
                // `codeAction/resolve` (`dataSupport`). Classify the shapes and
                // resolve the deferred ones, recording the raw results; no
                // command is ever executed.
                if method == "textDocument/codeAction" && captured.error.is_none() {
                    let returned = captured.result.as_array().cloned().unwrap_or_default();
                    for action in &returned {
                        actions.classify(action);
                    }
                    for action in returned.into_iter().filter(is_unresolved).take(5) {
                        let title = action
                            .get("title")
                            .and_then(Value::as_str)
                            .unwrap_or("action")
                            .to_string();
                        let mut resolve_meta = meta.clone();
                        resolve_meta.query = title;
                        index += 1;
                        let resolved_action = capture(&handle, "codeAction/resolve", action);
                        write_raw(
                            &raw,
                            index,
                            &resolve_meta,
                            "codeAction/resolve",
                            &Value::Null,
                            &resolved_action,
                        )?;
                        total += 1;
                        if resolved_action.error.is_none() {
                            ok += 1;
                        } else {
                            errored += 1;
                        }
                        if resolved_action.error.is_none()
                            && resolved_action.result.get("edit").is_some()
                        {
                            actions.resolved += 1;
                        }
                    }
                }
            }
        }
    }

    // Within-process warm repeat, not a cold-vs-warm process comparison: re-run
    // workspace/symbol + definition for every annotation after the server has
    // answered the whole set, and report the result. Partial evidence toward
    // experiment F, labelled as such rather than claimed as the experiment.
    let warm_started = Instant::now();
    let mut warm_total = 0usize;
    let mut warm_nonempty = 0usize;
    for (_, text, uri) in &sources {
        for annotation in parse_annotations(text) {
            let Some(resolved) = resolve_annotation(text, &annotation) else {
                continue;
            };
            let target = position(text, resolved.target_offset);
            for (method, params) in [
                ("workspace/symbol", json!({ "query": annotation.query })),
                (
                    "textDocument/definition",
                    json!({ "textDocument": { "uri": uri }, "position": target }),
                ),
            ] {
                let captured = capture(&handle, method, params);
                warm_total += 1;
                if captured.error.is_none() && is_nonempty(&captured.result) {
                    warm_nonempty += 1;
                }
            }
        }
    }
    let warm_elapsed_ms = warm_started.elapsed().as_millis();
    eprintln!(
        "=== {}: warm repeat {warm_nonempty}/{warm_total} non-empty in {warm_elapsed_ms}ms",
        server.slug()
    );

    let manifest = json!({
        "server": server.slug(),
        "command": command,
        "args": args,
        "fixture": fixture.to_string_lossy(),
        "captured_at_epoch_s": epoch_secs(),
        "initialize": initialize,
        "serverInfo": server_info,
        "initialize_elapsed_ms": initialize_elapsed_ms,
        "readiness": {
            "strategy": readiness.strategy,
            "elapsed_ms": readiness.elapsed.as_millis(),
            "quiescent": readiness.quiescent,
            "evidence": readiness.evidence,
            "post_open_serverStatus": post_open_status,
            "post_open_note": "readiness before didOpen is not necessarily query-phase readiness; this status is observed after the opens",
        },
        "first_useful_result_ms": first_useful_ms,
        "warm_repeat": {
            "queries": warm_total,
            "nonempty": warm_nonempty,
            "elapsed_ms": warm_elapsed_ms,
            "note": "within-process repeat after the main pass; NOT the cold-vs-warm process experiment (F)",
        },
        "code_actions": {
            "edit_bearing": actions.edit_bearing,
            "command_only": actions.command_only,
            "unresolved": actions.unresolved,
            "resolved_via_resolve": actions.resolved,
            "other": actions.other,
        },
        "sources": sources.iter().map(|(p, _, u)| json!({"path": p.to_string_lossy(), "uri": u})).collect::<Vec<_>>(),
        "queries": {
            "total": total,
            "ok": ok,
            "error": errored,
            "nonempty": nonempty,
        },
        "events": handle.capture_events()?,
        "notes": [
            "read-only: no edit applied, no code-action command executed, workspace/applyEdit declined",
            "raw/ holds the unmodified result for every query",
            "experiment F is only partially evidenced: warm_repeat is a within-process repeat, not a cold vs warm process comparison",
        ],
    });

    handle.shutdown()?;

    fs::write(
        dir.join("manifest.json"),
        serde_json::to_vec_pretty(&manifest)?,
    )?;

    Ok(Report {
        server,
        readiness,
        total,
        ok,
        errored,
        nonempty,
        events: manifest["events"].as_array().map(Vec::len).unwrap_or(0),
        dir,
    })
}

#[cfg(test)]
mod tests;
