//! Where a workspace's configuration comes from: the `structural.toml` at
//! its root, or the embedder's typed source in its place
//! (`docs/servers.md`).

use super::fixtures::fixture;
use super::*;
use crate::config::{Config, ServerEntry};

#[test]
fn a_broken_config_file_is_reported_not_fatal() {
    let dir =
        std::env::temp_dir().join(format!("structural-host-badconfig-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // A present but unusable file: `cmd` is not a key any server entry takes.
    std::fs::write(
        dir.join("structural.toml"),
        "[servers.thing]\ncmd = [\"x\"]\n",
    )
    .unwrap();

    let host = Host::new();
    let session = bound(&host, "a", &dir);

    // The workspace still binds — the MCP surface must come up — and every
    // call says why it cannot answer.
    let error = session
        .workspace()
        .select(None, None)
        .unwrap_err()
        .to_string();
    assert!(error.contains("structural.toml"), "{error}");
    assert!(error.contains("cmd"), "{error}");
    host.shutdown();
}

/// A typed configuration replaces the file: the mock runs from entries
/// built in code while the root's `structural.toml` is one that would not
/// parse, and the source is asked once per workspace, with its canonical
/// root, however many agents bind there.
#[test]
fn a_config_source_replaces_the_file() {
    let Some(fixture) = fixture("config-source") else {
        return;
    };
    let mock = |label: &str| ServerEntry {
        command: vec![
            fixture.python.clone(),
            fixture.script.display().to_string(),
            fixture.log.display().to_string(),
            label.to_string(),
        ],
        ..ServerEntry::default()
    };
    let config = Config {
        servers: [
            ("rust-analyzer".to_string(), mock("rust-analyzer")),
            ("pyright".to_string(), mock("pyright")),
        ]
        .into(),
        ..Config::default()
    };
    std::fs::write(
        fixture.root.join(crate::config::FILE_NAME),
        "[servers.thing]\ncmd = [\"x\"]\n",
    )
    .unwrap();
    let asked = Arc::new(Mutex::new(Vec::new()));
    let host = Host::new().with_config_source({
        let asked = Arc::clone(&asked);
        move |root| {
            asked.lock().unwrap().push(root.to_path_buf());
            Ok(config.clone())
        }
    });

    let a = bound(&host, "a", &fixture.root);
    let _b = bound(&host, "b", &fixture.root);

    assert_eq!(*asked.lock().unwrap(), [workspace_key(&fixture.root)]);
    assert_eq!(fixture.launches().len(), 2, "both mock servers started");
    assert!(a.workspace().select(None, None).is_ok());
    host.shutdown();
}

/// An error from the source is the workspace's configuration error, as a
/// file that does not parse is: the workspace binds, nothing starts, and
/// every call carries the source's message.
#[test]
fn a_config_source_error_is_reported_not_fatal() {
    let Some(fixture) = fixture("config-source-error") else {
        return;
    };
    let host = Host::new()
        .with_config_source(|_| anyhow::bail!("[structural] in efference.toml: no such key"));

    let session = bound(&host, "a", &fixture.root);

    let error = session
        .workspace()
        .select(None, None)
        .unwrap_err()
        .to_string();
    assert!(error.contains("efference.toml: no such key"), "{error}");
    assert!(fixture.launches().is_empty(), "nothing starts");
    host.shutdown();
}
