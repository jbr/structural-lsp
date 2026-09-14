//! `structural.toml` at the workspace root: which language servers to run.
//!
//! ```toml
//! [servers.rust-analyzer]          # a built-in: override its command or backstop
//! command = ["/opt/ra/rust-analyzer"]
//! backstop_secs = 180
//!
//! [servers.gopls]                  # any other name: a generic server
//! command = ["gopls", "serve"]
//! languages = ["go"]               # language ids; extensions from the table
//!                                  # below, or:
//! extensions = { go = ["go"] }
//! ```
//!
//! Built-in adapters are on by default; an entry with a built-in's name
//! overrides its command, languages, or backstop and keeps everything else
//! (readiness policy, capabilities, query rewriting). Absent file: the
//! built-ins alone.

use crate::lsp::adapter::{self, Adapter, DEFAULT_BACKSTOP, Language, configured::Configured};
use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

pub const FILE_NAME: &str = "structural.toml";

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub servers: BTreeMap<String, ServerEntry>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerEntry {
    /// argv, executable first. Required for a generic server.
    #[serde(default)]
    pub command: Vec<String>,
    /// Language ids. Required for a generic server.
    #[serde(default)]
    pub languages: Vec<String>,
    /// Extensions per language id, when the defaults below do not cover it.
    #[serde(default)]
    pub extensions: BTreeMap<String, Vec<String>>,
    pub backstop_secs: Option<u64>,
}

/// Extensions for language ids a user is likely to name without spelling
/// them out. Anything else needs `extensions`.
fn default_extensions(language: &str) -> Option<&'static [&'static str]> {
    Some(match language {
        "rust" => &["rs"],
        "python" => &["py", "pyi"],
        "typescript" => &["ts", "tsx", "mts", "cts"],
        "javascript" => &["js", "jsx", "mjs", "cjs"],
        "go" => &["go"],
        "c" => &["c", "h"],
        "cpp" => &["cpp", "cc", "cxx", "hpp", "hh"],
        "ruby" => &["rb"],
        "java" => &["java"],
        "kotlin" => &["kt", "kts"],
        "zig" => &["zig"],
        _ => return None,
    })
}

impl Config {
    /// The config at `root`, or the empty default when there is none. A
    /// present but unparsable file is an error: silently ignoring it would
    /// run the wrong servers.
    pub fn load(root: &Path) -> Result<Self> {
        let path = root.join(FILE_NAME);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
        };
        Self::parse(&text).with_context(|| format!("parsing {}", path.display()))
    }

    pub fn parse(text: &str) -> Result<Self> {
        Ok(toml::from_str(text)?)
    }

    /// The adapters to run: every built-in (overridden where named) plus
    /// every other entry as a generic server.
    pub fn adapters(self) -> Result<Vec<Box<dyn Adapter>>> {
        let mut entries = self.servers;
        let mut adapters: Vec<Box<dyn Adapter>> = Vec::new();
        for builtin in adapter::builtins() {
            match entries.remove(builtin.name()) {
                Some(entry) => adapters.push(Box::new(override_builtin(builtin, entry)?)),
                None => adapters.push(builtin),
            }
        }
        for (name, entry) in entries {
            adapters.push(Box::new(generic(name, entry)?));
        }
        Ok(adapters)
    }
}

fn languages_of(name: &str, entry: &ServerEntry) -> Result<Vec<Language>> {
    entry
        .languages
        .iter()
        .map(|id| {
            let extensions: Vec<String> = match entry.extensions.get(id) {
                Some(extensions) => extensions.clone(),
                None => default_extensions(id)
                    .map(|e| e.iter().map(|s| s.to_string()).collect())
                    .with_context(|| {
                        format!(
                            "[servers.{name}]: no known file extensions for language `{id}`; \
                             add `extensions = {{ {id} = [\"…\"] }}`"
                        )
                    })?,
            };
            Ok(Language {
                id: id.clone(),
                extensions,
            })
        })
        .collect()
}

fn override_builtin(builtin: Box<dyn Adapter>, entry: ServerEntry) -> Result<Configured> {
    let name = builtin.name().to_string();
    let command = if entry.command.is_empty() {
        builtin.command()
    } else {
        entry.command.clone()
    };
    let languages = if entry.languages.is_empty() {
        builtin.languages()
    } else {
        languages_of(&name, &entry)?
    };
    let backstop = entry
        .backstop_secs
        .map(Duration::from_secs)
        .unwrap_or_else(|| builtin.backstop());
    Ok(Configured {
        name,
        command,
        languages,
        backstop,
        base: Some(builtin),
    })
}

fn generic(name: String, entry: ServerEntry) -> Result<Configured> {
    if entry.command.is_empty() {
        bail!("[servers.{name}]: `command` is required for a server that is not built in");
    }
    if entry.languages.is_empty() {
        bail!("[servers.{name}]: `languages` is required for a server that is not built in");
    }
    let languages = languages_of(&name, &entry)?;
    Ok(Configured {
        name,
        command: entry.command,
        languages,
        backstop: entry
            .backstop_secs
            .map(Duration::from_secs)
            .unwrap_or(DEFAULT_BACKSTOP),
        base: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_config_is_the_builtins() {
        let adapters = Config::default().adapters().unwrap();
        let names: Vec<&str> = adapters.iter().map(|a| a.name()).collect();
        assert_eq!(names, ["rust-analyzer", "pyright"]);
    }

    #[test]
    fn a_builtin_entry_overrides_command_and_backstop_only() {
        let config = Config::parse(
            r#"
            [servers.rust-analyzer]
            command = ["/opt/ra/rust-analyzer", "--log-file", "/tmp/ra.log"]
            backstop_secs = 180
            "#,
        )
        .unwrap();
        let adapters = config.adapters().unwrap();
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
        let adapters = config.adapters().unwrap();
        let gopls = adapters.iter().find(|a| a.name() == "gopls").unwrap();
        assert_eq!(gopls.command(), ["gopls", "serve"]);
        assert_eq!(gopls.languages(), [Language::new("go", &["go"])]);
        assert_eq!(gopls.backstop(), DEFAULT_BACKSTOP);

        let missing = Config::parse("[servers.thing]\nlanguages = [\"go\"]\n")
            .unwrap()
            .adapters()
            .err()
            .expect("command is required");
        assert!(
            missing.to_string().contains("`command` is required"),
            "{missing}"
        );

        let unknown =
            Config::parse("[servers.thing]\ncommand = [\"thing\"]\nlanguages = [\"brainfuck\"]\n")
                .unwrap()
                .adapters()
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
        .adapters()
        .unwrap();
        assert_eq!(explicit.last().unwrap().languages()[0].extensions, ["bf"]);
    }

    #[test]
    fn unknown_keys_are_rejected() {
        let error = Config::parse("[servers.gopls]\ncmd = [\"gopls\"]\n").unwrap_err();
        assert!(error.to_string().contains("cmd"), "{error}");
    }
}
