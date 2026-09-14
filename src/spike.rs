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
use structural::lsp::client::Client;

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

    fn language_id(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::Python => "python",
        }
    }

    fn command(self) -> (&'static str, Vec<String>) {
        match self {
            Self::Rust => ("rust-analyzer", Vec::new()),
            Self::Python => ("pyright-langserver", vec!["--stdio".to_string()]),
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
    /// Longest the spike waits for rust-analyzer's `serverStatus` quiescence.
    pub quiesce: Duration,
    /// Bounded wait for pyright, which has no readiness signal.
    pub pyright_wait: Duration,
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

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum AnnKind {
    Decl,
    Expr,
}

impl AnnKind {
    fn label(self) -> &'static str {
        match self {
            Self::Decl => "decl",
            Self::Expr => "expr",
        }
    }
}

struct Annotation {
    kind: AnnKind,
    /// The symbol to resolve / query (the first field before `|`).
    query: String,
    /// The literal source text to locate (the second field after `|`).
    selection: String,
    /// Byte offset of the annotation line's start.
    line_start: usize,
    /// Byte offset just past the annotation's own line: the selection must be
    /// found within `search_from..search_until`.
    search_from: usize,
    /// Byte offset where this annotation's own region ends: the next
    /// annotation's line start, or end of file. Bounding the lookup here keeps
    /// a missing selection from silently matching a *later* annotation's text.
    search_until: usize,
    source_line: usize,
}

struct Resolved {
    selection_start: usize,
    selection_end: usize,
    target_offset: usize,
    /// How `target_offset` was chosen: `"query-in-selection"` when the query
    /// token was found inside the selection, `"selection-start"` when it was
    /// not — in which case the query is *not* confirmed to occur there, and the
    /// caller must read the provenance rather than assume the target is right.
    target_provenance: &'static str,
}

/// Metadata for one annotation, written alongside every query it produced.
#[derive(Clone)]
struct AnnotationMeta {
    kind: &'static str,
    query: String,
    selection: String,
    source_line: usize,
    selection_range: Option<(Position, Position)>,
    target: Option<Position>,
    target_provenance: &'static str,
}

impl AnnotationMeta {
    fn json(&self) -> Value {
        json!({
            "kind": self.kind,
            "query": self.query,
            "selection": self.selection,
            "source_line": self.source_line,
            "selection_range": self.selection_range,
            "target": self.target,
            "target_provenance": self.target_provenance,
        })
    }
}

/// The outcome of one request, captured uniformly for the raw log.
struct Captured {
    result: Value,
    error: Option<String>,
    elapsed_ms: u128,
}

/// Counts of code-action shapes seen, so a menu that is all commands is
/// distinguishable from one whose edits are deferred behind `codeAction/resolve`.
#[derive(Default)]
struct ActionTally {
    edit_bearing: usize,
    command_only: usize,
    unresolved: usize,
    resolved: usize,
    other: usize,
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

    let (command, args) = server.command();
    eprintln!(
        "=== {}: spawning `{command} {}`",
        server.slug(),
        args.join(" ")
    );
    let mut client = Client::spawn(command, &args)?;

    let root_uri = url::Url::from_directory_path(&fixture)
        .map_err(|_| anyhow!("invalid fixture path {}", fixture.display()))?
        .to_string();

    let started = Instant::now();
    let initialize = client.request(
        "initialize",
        initialize_params(&root_uri, &fixture, server),
        Duration::from_secs(60),
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

    client.notify("initialized", json!({}))?;

    let readiness = wait_for_ready(&mut client, server, options)?;
    eprintln!(
        "=== {}: readiness after {:.1}s (quiescent={}): {}",
        server.slug(),
        readiness.elapsed.as_secs_f32(),
        readiness.quiescent,
        readiness.strategy
    );

    // Open every fixture source file, then give the server a moment.
    let sources = collect_sources(&fixture)?;
    for (path, text, uri) in &sources {
        client.notify(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": uri,
                    "languageId": server.language_id(),
                    "version": 1,
                    "text": text,
                }
            }),
        )?;
        eprintln!("=== {}: didOpen {}", server.slug(), path.display());
    }
    client.pump(Duration::from_millis(500))?;

    // Readiness observed *before* didOpen is not necessarily query-phase
    // readiness — rust-analyzer's quiescence can precede source loading. Record
    // what the server reports now, after the opens.
    let post_open_status = latest_server_status(&mut client, Duration::from_millis(500))?;
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
                let captured = capture(&mut client, method, params.clone(), options.query_timeout);
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
                        let resolved_action = capture(
                            &mut client,
                            "codeAction/resolve",
                            action,
                            options.query_timeout,
                        );
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
                let captured = capture(&mut client, method, params, options.query_timeout);
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
        "events": client.events,
        "notes": [
            "read-only: no edit applied, no code-action command executed, workspace/applyEdit declined",
            "raw/ holds the unmodified result for every query",
            "experiment F is only partially evidenced: warm_repeat is a within-process repeat, not a cold vs warm process comparison",
        ],
    });

    client.shutdown()?;

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

/// Run one request, capturing result, error, and latency uniformly.
fn capture(client: &mut Client, method: &str, params: Value, timeout: Duration) -> Captured {
    let began = Instant::now();
    let (result, error) = match client.request(method, params, timeout) {
        Ok(value) => (value, None),
        Err(error) => (Value::Null, Some(format!("{error:#}"))),
    };
    Captured {
        result,
        error,
        elapsed_ms: began.elapsed().as_millis(),
    }
}

/// Pump briefly and return the most recent `experimental/serverStatus` params,
/// or `Null` when the server advertises none (pyright) or none is new.
fn latest_server_status(client: &mut Client, wait: Duration) -> Result<Value> {
    client.pump(wait)?;
    Ok(client
        .events
        .iter()
        .rev()
        .find(|event| {
            event.get("method").and_then(Value::as_str) == Some("experimental/serverStatus")
        })
        .and_then(|event| event.get("params").cloned())
        .unwrap_or(Value::Null))
}

/// Write one captured query to `raw/`, unmodified.
fn write_raw(
    raw: &Path,
    index: usize,
    meta: &AnnotationMeta,
    method: &str,
    params: &Value,
    captured: &Captured,
) -> Result<()> {
    let file = raw.join(format!(
        "{index:03}-{}-{}-{}.json",
        meta.kind,
        slug(&meta.query),
        slug(method)
    ));
    fs::write(
        &file,
        serde_json::to_vec_pretty(&json!({
            "annotation": meta.json(),
            "method": method,
            "params": params,
            "elapsed_ms": captured.elapsed_ms,
            "ok": captured.error.is_none(),
            "error": captured.error,
            "result": captured.result,
        }))?,
    )?;
    Ok(())
}

/// A code action whose edit is deferred behind `codeAction/resolve`.
fn is_unresolved(action: &Value) -> bool {
    action.get("data").is_some() && action.get("edit").is_none()
}

impl ActionTally {
    fn classify(&mut self, action: &Value) {
        if action.get("edit").is_some() {
            self.edit_bearing += 1;
        } else if action.get("command").is_some() {
            self.command_only += 1;
        } else if action.get("data").is_some() {
            self.unresolved += 1;
        } else {
            self.other += 1;
        }
    }
}

fn initialize_params(root_uri: &str, root: &Path, server: Server) -> Value {
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

fn wait_for_ready(client: &mut Client, server: Server, options: &Options) -> Result<Readiness> {
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

fn collect_sources(root: &Path) -> Result<Vec<(PathBuf, String, String)>> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).with_context(|| format!("reading {}", dir.display()))? {
            let path = entry?.path();
            if path.is_dir() {
                // Skip build output; never index it.
                if path.file_name().is_some_and(|n| n == "target") {
                    continue;
                }
                stack.push(path);
            } else if matches!(
                path.extension().and_then(|e| e.to_str()),
                Some("rs") | Some("py")
            ) {
                let text = fs::read_to_string(&path)?;
                let uri = url::Url::from_file_path(&path)
                    .map_err(|_| anyhow!("invalid file path {}", path.display()))?
                    .to_string();
                files.push((path, text, uri));
            }
        }
    }
    files.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(files)
}

fn parse_annotations(text: &str) -> Vec<Annotation> {
    let mut out = Vec::new();
    let mut offset = 0usize;
    for (index, raw) in text.split_inclusive('\n').enumerate() {
        let line_start = offset;
        let line = raw.trim_end_matches(['\r', '\n']).trim_start();
        // Rust uses `//` comments, Python uses `#`; both carry the same annotation.
        let decl = line
            .strip_prefix("// @decl ")
            .or_else(|| line.strip_prefix("# @decl "));
        let expr = line
            .strip_prefix("// @expr ")
            .or_else(|| line.strip_prefix("# @expr "));
        let (kind, rest) = if let Some(rest) = decl {
            (AnnKind::Decl, rest)
        } else if let Some(rest) = expr {
            (AnnKind::Expr, rest)
        } else {
            offset += raw.len();
            continue;
        };
        if let Some((query, selection)) = rest.split_once('|') {
            let query = query.trim();
            let selection = selection.trim();
            if !query.is_empty() && !selection.is_empty() {
                out.push(Annotation {
                    kind,
                    query: query.to_string(),
                    selection: selection.to_string(),
                    line_start,
                    search_from: offset + raw.len(),
                    // Narrowed below, once every annotation's start is known.
                    search_until: text.len(),
                    source_line: index + 1,
                });
            }
        }
        offset += raw.len();
    }
    // An annotation's lookup region ends where the next annotation begins, so
    // its selection cannot be satisfied by a later annotation's source text.
    for index in 0..out.len() {
        out[index].search_until = out
            .get(index + 1)
            .map(|next| next.line_start)
            .unwrap_or(text.len());
    }
    out
}

/// Locate the annotation's `selection` within its own region (bounded above by
/// the next annotation), and the `query` token within that selection.
///
/// `None` means the selection text is not present in the region. If the query
/// token is not inside the selection, the target falls back to the selection
/// start with `target_provenance = "selection-start"`; it is never searched for
/// beyond the selection, which would silently resolve some later occurrence.
fn resolve_annotation(text: &str, annotation: &Annotation) -> Option<Resolved> {
    let from = annotation.search_from.min(text.len());
    let until = annotation.search_until.clamp(from, text.len());
    let relative = text[from..until].find(&annotation.selection)?;
    let selection_start = from + relative;
    let selection_end = selection_start + annotation.selection.len();

    let (target_offset, target_provenance) = match annotation.selection.find(&annotation.query) {
        Some(relative) => (selection_start + relative, "query-in-selection"),
        None => (selection_start, "selection-start"),
    };

    Some(Resolved {
        selection_start,
        selection_end,
        target_offset,
        target_provenance,
    })
}

/// Byte offset to an LSP position. LSP positions are UTF-16 code units, which
/// only diverges from bytes on non-ASCII input; the fixtures are ASCII but the
/// conversion is done honestly.
fn position(text: &str, offset: usize) -> Position {
    let mut line = 0u32;
    let mut character = 0u32;
    for (index, ch) in text.char_indices() {
        if index >= offset {
            break;
        }
        if ch == '\n' {
            line += 1;
            character = 0;
        } else {
            character += ch.len_utf16() as u32;
        }
    }
    Position { line, character }
}

fn is_nonempty(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Array(items) => !items.is_empty(),
        Value::Object(map) => !map.is_empty(),
        Value::Bool(_) | Value::Number(_) | Value::String(_) => true,
    }
}

fn slug(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        out.push_str("empty");
    }
    out
}

fn epoch_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_both_annotation_kinds() {
        let text =
            "// @decl Config | Config\npub struct Config;\n// @expr limit | self.limit * 2\nx\n";
        let annotations = parse_annotations(text);
        assert_eq!(annotations.len(), 2);
        assert_eq!(annotations[0].kind, AnnKind::Decl);
        assert_eq!(annotations[0].query, "Config");
        assert_eq!(annotations[0].selection, "Config");
        assert_eq!(annotations[1].kind, AnnKind::Expr);
        assert_eq!(annotations[1].query, "limit");
        assert_eq!(annotations[1].selection, "self.limit * 2");
    }

    #[test]
    fn resolves_selection_and_query_token_after_the_annotation() {
        let text = "// @expr doubled | config.doubled()\nlet x = config.doubled();\n";
        let annotations = parse_annotations(text);
        let resolved = resolve_annotation(text, &annotations[0]).unwrap();
        assert_eq!(
            &text[resolved.selection_start..resolved.selection_end],
            "config.doubled()"
        );
        assert_eq!(
            &text[resolved.target_offset..resolved.target_offset + 7],
            "doubled"
        );
        assert_eq!(resolved.target_provenance, "query-in-selection");
    }

    #[test]
    fn query_absent_from_selection_falls_back_to_selection_start_with_provenance() {
        // `beyond` does not occur in the selection but does occur on a later
        // line. The target must be the selection start, with provenance saying
        // the query was not actually located — never the later occurrence.
        let text = "// @expr beyond | alpha()\nlet x = alpha();\nlet beyond = 1;\n";
        let annotations = parse_annotations(text);
        let resolved = resolve_annotation(text, &annotations[0]).unwrap();
        assert_eq!(
            &text[resolved.selection_start..resolved.selection_end],
            "alpha()"
        );
        assert_eq!(resolved.target_offset, resolved.selection_start);
        assert_eq!(resolved.target_provenance, "selection-start");
    }

    #[test]
    fn selection_missing_from_the_annotations_region_is_unresolved() {
        // The first annotation's selection (`alpha`) only appears after the
        // second annotation begins, so it must not match that later text.
        let text = "// @decl first | alpha\n// @decl second | second\nsecond\n";
        let annotations = parse_annotations(text);
        assert!(
            resolve_annotation(text, &annotations[0]).is_none(),
            "a selection past its own region must not resolve against later source"
        );
        // The second annotation's selection is in its own region, so it does.
        let resolved = resolve_annotation(text, &annotations[1]).unwrap();
        assert_eq!(
            &text[resolved.selection_start..resolved.selection_end],
            "second"
        );
    }

    #[test]
    fn declaration_selection_resolves_within_its_own_region() {
        let text = "// @decl smoke | smoke\n#[test]\nfn smoke() {}\n";
        let annotations = parse_annotations(text);
        let resolved = resolve_annotation(text, &annotations[0]).unwrap();
        assert_eq!(
            &text[resolved.selection_start..resolved.selection_end],
            "smoke"
        );
        assert_eq!(resolved.target_offset, resolved.selection_start);
        assert_eq!(resolved.target_provenance, "query-in-selection");
    }

    #[test]
    fn positions_count_utf16_units() {
        let text = "a\n\u{1F600}b";
        // The emoji is one scalar but two UTF-16 code units.
        assert_eq!(
            position(text, 0),
            Position {
                line: 0,
                character: 0
            }
        );
        assert_eq!(
            position(text, 2),
            Position {
                line: 1,
                character: 0
            }
        );
        assert_eq!(
            position(text, 6),
            Position {
                line: 1,
                character: 2
            }
        );
    }

    #[test]
    fn nonempty_treats_empty_collections_as_empty() {
        assert!(!is_nonempty(&Value::Null));
        assert!(!is_nonempty(&json!([])));
        assert!(!is_nonempty(&json!({})));
        assert!(is_nonempty(&json!([1])));
        assert!(is_nonempty(&json!(false))); // a real boolean answer is information
    }
}
