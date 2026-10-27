//! The schema the key types publish, and the committed copy of it at
//! `schemas/config.json` that editors read (`docs/servers.md`).
//!
//! `STRUCTURAL_WRITE_SCHEMA=1 cargo test schema` regenerates the file.

use super::*;
use crate::lsp::adapter::DEFAULT_BACKSTOP;
use schemars::generate::SchemaSettings;
use serde_json::Map;
use std::path::PathBuf;

/// The committed file, relative to the crate root.
const COMMITTED: &str = "schemas/config.json";

/// The schema's title, which, like every description in it, does not name
/// the tool: embedded, the same keys are another product's.
const TITLE: &str = "language server configuration";

/// What `schemas/config.json` holds: the schema for [`Config`], tidied for
/// TOML editors, rendered as pretty JSON with a trailing newline.
fn published() -> String {
    let mut value = SchemaSettings::default()
        .for_deserialize()
        .into_generator()
        .into_root_schema_for::<Config>()
        .to_value();
    tidy(&mut value);
    value["title"] = TITLE.into();
    let mut text = serde_json::to_string_pretty(&value).unwrap();
    text.push('\n');
    text
}

/// Rewrite what schemars emits into what a TOML editor (Taplo, Tombi) reads
/// well, at every depth — the same rewriting efference applies to its own
/// schemas, so a key reads alike in both:
///
/// - TOML has no null, so an `Option` field loses the `null` in its type
///   and the `default: null` its `#[serde(default)]` produces.
/// - A numeric `format` (`uint64`) is a Rust width, not a format either
///   editor checks.
fn tidy(value: &mut Value) {
    match value {
        Value::Object(object) => {
            drop_null(object);
            if matches!(object.get("type"), Some(Value::String(t)) if t == "integer" || t == "number")
            {
                object.remove("format");
            }
            object.values_mut().for_each(tidy);
        }
        Value::Array(items) => items.iter_mut().for_each(tidy),
        _ => {}
    }
}

fn drop_null(object: &mut Map<String, Value>) {
    if object.get("default") == Some(&Value::Null) {
        object.remove("default");
    }
    if let Some(Value::Array(types)) = object.get_mut("type") {
        types.retain(|t| t != "null");
        if let [only] = types.as_slice() {
            let only = only.clone();
            object.insert("type".into(), only);
        }
    }
}

/// Every `description` in the schema, at any depth.
fn descriptions(value: &Value, found: &mut Vec<String>) {
    match value {
        Value::Object(object) => {
            if let Some(Value::String(description)) = object.get("description") {
                found.push(description.clone());
            }
            object.values().for_each(|value| descriptions(value, found));
        }
        Value::Array(items) => items.iter().for_each(|value| descriptions(value, found)),
        _ => {}
    }
}

#[test]
fn the_committed_schema_matches_the_types() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(COMMITTED);
    let generated = published();
    if std::env::var_os("STRUCTURAL_WRITE_SCHEMA").is_some() {
        std::fs::write(&path, &generated).unwrap();
        return;
    }
    let committed = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        committed == generated,
        "{COMMITTED} is not what the configuration types generate; \
         regenerate it with `STRUCTURAL_WRITE_SCHEMA=1 cargo test schema`"
    );
}

/// The shape an editor and an embedder rely on: every key kebab-case and
/// described, unknown keys refused, no text naming a repository path or the
/// tool, and the defaults the hover text states equal to the code's.
#[test]
fn the_schema_describes_every_key_for_the_file() {
    let schema: Value = serde_json::from_str(&published()).unwrap();
    let entry = &schema["$defs"]["ServerEntry"];
    for object in [&schema, entry] {
        assert_eq!(object["additionalProperties"], false, "{object:#}");
        assert!(object["description"].is_string(), "{object:#}");
        for (key, property) in object["properties"].as_object().unwrap() {
            assert!(!key.contains('_'), "`{key}` is not kebab-case");
            assert!(
                property["description"].is_string(),
                "`{key}` has no description"
            );
        }
    }
    let mut found = vec![schema["title"].as_str().unwrap().to_string()];
    descriptions(&schema, &mut found);
    for text in &found {
        for path in ["docs/", "src/", ".rs", ".md"] {
            assert!(!text.contains(path), "names a repository path: {text}");
        }
        assert!(
            !text.to_lowercase().contains("structural"),
            "names the tool, which embedded is another product's: {text}"
        );
    }

    let described = |object: &Value, key: &str| {
        object["properties"][key]["description"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let idle = DEFAULT_IDLE_EVICTION.as_secs();
    assert!(described(&schema, "idle-eviction-secs").contains(&format!("`{idle}`")));
    let backstop = DEFAULT_BACKSTOP.as_secs();
    assert!(described(entry, "backstop-secs").contains(&format!("`{backstop}`")));
}
