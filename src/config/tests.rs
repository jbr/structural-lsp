//! `structural.toml` read into a layout.

use super::*;

#[test]
fn absent_config_is_the_builtins() {
    let adapters = Config::default().layout().unwrap().adapters;
    let names: Vec<&str> = adapters.iter().map(|a| a.name()).collect();
    assert_eq!(names, ["rust-analyzer", "pyright"]);
}

#[test]
fn a_builtin_entry_overrides_command_and_backstop_only() {
    let config = Config::parse(
        r#"
        [servers.rust-analyzer]
        command = ["/opt/ra/rust-analyzer", "--log-file", "/tmp/ra.log"]
        backstop-secs = 180
        "#,
    )
    .unwrap();
    let adapters = config.layout().unwrap().adapters;
    let ra = &adapters[0];
    assert_eq!(ra.name(), "rust-analyzer");
    assert_eq!(ra.command()[0], "/opt/ra/rust-analyzer");
    assert_eq!(ra.backstop(), Duration::from_secs(180));
    assert_eq!(ra.languages()[0].extensions, ["rs"]);
    // The built-in's knowledge is kept.
    assert!(
        ra.initialization_options()
            .pointer("/workspace/symbol/search/kind")
            .is_some()
    );
    assert!(ra.install_hint().contains("rustup"));
}

#[test]
fn a_generic_entry_needs_command_and_languages() {
    let config = Config::parse(
        r#"
        [servers.gopls]
        command = ["gopls", "serve"]
        languages = ["go"]
        "#,
    )
    .unwrap();
    let adapters = config.layout().unwrap().adapters;
    let gopls = adapters.iter().find(|a| a.name() == "gopls").unwrap();
    assert_eq!(gopls.command(), ["gopls", "serve"]);
    assert_eq!(gopls.languages(), [Language::new("go", &["go"])]);
    assert_eq!(gopls.backstop(), DEFAULT_BACKSTOP);

    let missing = Config::parse("[servers.thing]\nlanguages = [\"go\"]\n")
        .unwrap()
        .layout()
        .err()
        .expect("command is required");
    assert!(
        missing.to_string().contains("`command` is required"),
        "{missing}"
    );

    let unknown =
        Config::parse("[servers.thing]\ncommand = [\"thing\"]\nlanguages = [\"brainfuck\"]\n")
            .unwrap()
            .layout()
            .err()
            .expect("unknown extensions are an error");
    assert!(
        unknown.to_string().contains("no known file extensions"),
        "{unknown}"
    );

    let explicit = Config::parse(
        "[servers.thing]\ncommand = [\"thing\"]\nlanguages = [\"brainfuck\"]\nextensions = { brainfuck = [\"bf\"] }\n",
    )
    .unwrap()
    .layout()
    .unwrap()
    .adapters;
    assert_eq!(explicit.last().unwrap().languages()[0].extensions, ["bf"]);
}

/// The file's spelling and a value built in code are one `Config`:
/// `settings` written as a TOML table reaches the server as the same
/// JSON an embedder would hand over.
#[test]
fn settings_from_toml_and_from_code_answer_alike() {
    let parsed = Config::parse(
        "[servers.deno]\ncommand = [\"deno\", \"lsp\"]\nlanguages = [\"typescript\"]\nsettings = { enable = true, lint = { rules = [\"a\"] } }\n",
    )
    .unwrap();
    let built = Config {
        servers: [(
            "deno".to_string(),
            ServerEntry {
                command: vec!["deno".to_string(), "lsp".to_string()],
                languages: vec!["typescript".to_string()],
                settings: Some(serde_json::json!({"enable": true, "lint": {"rules": ["a"]}})),
                ..ServerEntry::default()
            },
        )]
        .into(),
        ..Config::default()
    };
    let answer = |config: Config| {
        config
            .layout()
            .unwrap()
            .adapter("deno")
            .unwrap()
            .workspace_configuration()
    };
    assert_eq!(answer(parsed), answer(built));
}

#[test]
fn unknown_keys_are_rejected() {
    let error = Config::parse("[servers.gopls]\ncmd = [\"gopls\"]\n").unwrap_err();
    assert!(error.to_string().contains("cmd"), "{error}");
    // A snake_case spelling is an unknown key, and the error names the
    // kebab-case one.
    let error = Config::parse("[servers.gopls]\nbackstop_secs = 1\n").unwrap_err();
    assert!(error.to_string().contains("`backstop-secs`"), "{error}");
}
