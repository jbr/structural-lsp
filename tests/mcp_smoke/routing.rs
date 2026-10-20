//! Routing one call: `workspace` answers about another project without
//! changing the caller's own, and `language` narrows which servers a call
//! starts and asks (`docs/ownership.md`). The real-server cases need
//! rust-analyzer and pyright and are gated; the stdio case runs a mock.

use crate::Server;
use crate::mock_scope::{MOCK_PY, TOOL_RS, python, quoted};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use structural::session::Session;
use structural::tools::{Definition, References};

/// Both fixtures in sibling directories of one scratch root.
fn siblings(tag: &str) -> PathBuf {
    let root =
        std::env::temp_dir().join(format!("structural-routing-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    crate::fixtures_in(&root, &["rust", "python"]);
    std::fs::canonicalize(root).unwrap()
}

fn definition(
    session: &Session,
    symbol: &str,
    path: Option<&str>,
    workspace: Option<&str>,
    language: Option<&str>,
) -> anyhow::Result<String> {
    Definition {
        symbol: Some(symbol.to_string()),
        path: path.map(str::to_string),
        anchor: None,
        focus: None,
        view: None,
        page: None,
        workspace: workspace.map(str::to_string),
        language: language.map(str::to_string),
    }
    .run(session)
}

fn ok(result: anyhow::Result<String>) -> String {
    result.unwrap_or_else(|error| panic!("{error:#}"))
}

#[test]
#[ignore = "spawns rust-analyzer and pyright; run explicitly with -- --ignored"]
fn workspace_answers_about_another_tree_from_its_own_view() {
    let root = siblings("elsewhere");
    let session = Session::new(root.join("rust"));

    // Relative to the caller's view, and rendered relative to the other
    // tree: the Python fixture's `choose`, at its own `helper.py`.
    let python = ok(definition(
        &session,
        "choose",
        None,
        Some("../python"),
        None,
    ));
    assert!(python.starts_with("def choose · helper.py:"), "{python}");
    assert!(
        python.contains("def choose(flag: bool) -> int:"),
        "{python}"
    );

    // `path` is a scope inside the named workspace, relative to it.
    let made = ok(definition(
        &session,
        "make",
        Some("main.py"),
        Some(root.join("python").to_str().unwrap()),
        None,
    ));
    assert!(made.starts_with("def make · main.py:"), "{made}");

    let uses = ok(References {
        symbol: Some("choose".to_string()),
        path: None,
        anchor: None,
        focus: None,
        within: None,
        page: None,
        workspace: Some("../python".to_string()),
        language: None,
    }
    .run(&session));
    assert!(
        uses.contains("· 2 references · 1 file\nmain.py\n"),
        "{uses}"
    );

    // The caller's own default is untouched by either call.
    let own = ok(definition(&session, "choose", None, None, None));
    assert!(own.starts_with("fn choose · src/lib.rs:"), "{own}");

    // Not a directory: an argument error naming `workspace`.
    let error = definition(&session, "choose", None, Some("src/lib.rs"), None)
        .unwrap_err()
        .to_string();
    assert!(
        error.starts_with("`workspace` names `src/lib.rs`"),
        "{error}"
    );
}

#[test]
#[ignore = "spawns rust-analyzer and pyright; run explicitly with -- --ignored"]
fn language_narrows_a_two_language_tree() {
    let root = siblings("language");
    let session = Session::new(root.clone());

    // Without a language both servers are asked, and two definitions are
    // listed rather than one picked.
    let both = ok(definition(&session, "choose", None, None, None));
    assert!(
        both.starts_with("ambiguous: 2 definitions of 'choose'"),
        "{both}"
    );

    // By id and by extension, each language's own answer alone.
    let python = ok(definition(&session, "choose", None, None, Some("python")));
    assert!(
        python.starts_with("def choose · python/helper.py:"),
        "{python}"
    );
    let rust = ok(definition(&session, "choose", None, None, Some("rs")));
    assert!(rust.starts_with("fn choose · rust/src/lib.rs:"), "{rust}");

    // A language that agrees with a file `path` changes nothing.
    let by_file = ok(definition(
        &session,
        "make",
        Some("python/main.py"),
        None,
        Some(".py"),
    ));
    assert!(
        by_file.starts_with("def make · python/main.py:"),
        "{by_file}"
    );

    // One that matches nothing here, and one a file contradicts, are
    // argument errors whatever is running.
    let unknown = definition(&session, "choose", None, None, Some("go"))
        .unwrap_err()
        .to_string();
    assert!(
        unknown.starts_with("`language: go` matches no language server here; languages here: python (.py, .pyi), rust (.rs)"),
        "{unknown}"
    );
    let contradicted = definition(&session, "make", Some("python/main.py"), None, Some("rust"))
        .unwrap_err()
        .to_string();
    assert!(
        contradicted.starts_with("`path` is a `py` file, which `language: rust` does not cover"),
        "{contradicted}"
    );
}

/// A tree served by the mock alone, its launches logged to `log`.
fn mock_tree(root: &Path, log: &Path) {
    let _ = std::fs::remove_dir_all(root);
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/tool.rs"), TOOL_RS).unwrap();
    let mock = root.with_extension("mock.py");
    std::fs::write(&mock, MOCK_PY).unwrap();
    let launch = format!(
        "echo start >> '{}'; exec '{}' '{}'",
        log.display(),
        python(),
        mock.display()
    );
    std::fs::write(
        root.join("structural.toml"),
        format!(
            concat!(
                "[servers.rust-analyzer]\ndisabled = true\n\n",
                "[servers.mock]\ncommand = [\"/bin/sh\", \"-c\", \"{}\"]\nlanguages = [\"rust\"]\n"
            ),
            quoted(Path::new(&launch))
        ),
    )
    .unwrap();
}

/// Two calls naming another `workspace` through the real front end share
/// one launch of that workspace's server: the MCP session binds the same
/// agent through its host rather than building a workspace per call. Runs
/// everywhere: the server is a mock.
#[test]
fn stdio_workspace_binds_through_the_host_once() {
    let base =
        std::env::temp_dir().join(format!("structural-routing-stdio-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&base).unwrap();
    let (home, other) = (base.join("home"), base.join("other"));
    let (home_log, other_log) = (base.join("home.log"), base.join("other.log"));
    mock_tree(&home, &home_log);
    mock_tree(&other, &other_log);
    let launches = |log: &Path| {
        std::fs::read_to_string(log)
            .unwrap_or_default()
            .lines()
            .count()
    };

    let mut server = Server::start(home.to_str().unwrap());
    let initialize = server.request(
        "initialize",
        json!({
            "protocolVersion": "2025-11-25",
            "capabilities": {},
            "clientInfo": { "name": "smoke", "version": "0" }
        }),
    );
    assert!(initialize.get("result").is_some(), "{initialize}");
    server.notify("notifications/initialized", json!({}));

    let mut call = |arguments: Value| -> String {
        let response = server.request(
            "tools/call",
            json!({ "name": "definition", "arguments": arguments }),
        );
        assert_ne!(
            response.pointer("/result/isError"),
            Some(&json!(true)),
            "{response}"
        );
        response
            .pointer("/result/content/0/text")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let expected = "function new · src/tool.rs:7\npub fn new(name: String) -> Self {\n        Self { name }\n    }\n";
    let other_dir = other.to_str().unwrap().to_string();
    for _ in 0..2 {
        let answer = call(json!({
            "symbol": "new", "path": "src/tool.rs", "workspace": other_dir
        }));
        assert_eq!(answer, expected);
    }
    assert_eq!(launches(&other_log), 1, "one launch, shared by both calls");
    // The default workspace still answers, and was never re-launched.
    assert_eq!(
        call(json!({ "symbol": "new", "path": "src/tool.rs" })),
        expected
    );
    assert_eq!(launches(&home_log), 1);

    server.close_stdin();
    assert!(server.wait_exit().success());
}
