//! rust-analyzer's extension views: the requests behind them and how their
//! results read. Method names and result shapes are from rust-analyzer's
//! `lsp-extensions.md`; nothing here is standard LSP.

use crate::lsp::adapter::ViewSite;
use anyhow::{Context, Result, bail};
use lsp_types::Position;
use serde::Deserialize;
use serde_json::{Value, json};
use std::path::Path;

mod derive;

pub const EXPANDED: &str = "expanded";
pub const RELATED_TESTS: &str = "related-tests";

pub fn names() -> Vec<&'static str> {
    vec![EXPANDED, RELATED_TESTS]
}

pub fn request(view: &str, uri: &str, position: Position) -> Option<(String, Value)> {
    let params = json!({ "textDocument": { "uri": uri }, "position": position });
    match view {
        EXPANDED => Some(("rust-analyzer/expandMacro".to_string(), params)),
        RELATED_TESTS => Some(("rust-analyzer/relatedTests".to_string(), params)),
        _ => None,
    }
}

pub fn render(
    view: &str,
    result: &Value,
    root: &Path,
    site: Option<ViewSite<'_>>,
) -> Result<String> {
    match view {
        EXPANDED => render_expansion(result, site),
        RELATED_TESTS => render_related_tests(result, root),
        _ => bail!("rust-analyzer has no view named `{view}`"),
    }
}

/// `{ name, expansion }`, or `null` when rust-analyzer expands nothing at the
/// position: not a macro call, or a derive it does not expand
/// (docs/servers.md).
fn render_expansion(result: &Value, site: Option<ViewSite<'_>>) -> Result<String> {
    #[derive(Deserialize)]
    struct Expansion {
        name: String,
        expansion: String,
    }
    if result.is_null() {
        if let Some(message) = site.and_then(derive::explain_empty) {
            return Ok(message);
        }
        return Ok("not applicable: the focus is not a macro call\n".to_string());
    }
    let expansion: Expansion =
        serde_json::from_value(result.clone()).context("parsing rust-analyzer/expandMacro")?;
    // rust-analyzer's `name` already carries the `!`.
    Ok(format!(
        "expanded {}\n{}\n",
        expansion.name.trim_end_matches('!'),
        expansion.expansion.trim_end()
    ))
}

/// `TestInfo[]`, each a `Runnable` with the exact cargo invocation.
fn render_related_tests(result: &Value, root: &Path) -> Result<String> {
    #[derive(Deserialize)]
    struct TestInfo {
        runnable: Runnable,
    }
    #[derive(Deserialize)]
    struct Runnable {
        label: String,
        kind: String,
        location: Option<lsp_types::LocationLink>,
        args: RunnableArgs,
    }
    #[derive(Deserialize, Default)]
    #[serde(rename_all = "camelCase")]
    struct RunnableArgs {
        #[serde(default)]
        cargo_args: Vec<String>,
        #[serde(default)]
        executable_args: Vec<String>,
        #[serde(default)]
        cwd: Option<String>,
    }
    let tests: Vec<TestInfo> =
        serde_json::from_value(result.clone()).context("parsing rust-analyzer/relatedTests")?;
    if tests.is_empty() {
        return Ok("no related tests\n".to_string());
    }
    let mut text = format!(
        "{} related test{}\n",
        tests.len(),
        if tests.len() == 1 { "" } else { "s" }
    );
    for test in tests {
        let runnable = test.runnable;
        text.push_str(&runnable.label);
        if let Some(location) = &runnable.location {
            text.push_str(&format!(
                " · {}",
                crate::tools::listing::location_line(
                    root,
                    &location.target_uri,
                    location.target_selection_range.start
                )
            ));
        }
        text.push('\n');
        if runnable.kind == "cargo" {
            let mut command: Vec<String> = vec!["cargo".to_string()];
            command.extend(runnable.args.cargo_args.iter().map(|arg| shell_quote(arg)));
            if !runnable.args.executable_args.is_empty() {
                command.push("--".to_string());
                command.extend(
                    runnable
                        .args
                        .executable_args
                        .iter()
                        .map(|arg| shell_quote(arg)),
                );
            }
            text.push_str(&format!("  {}", command.join(" ")));
            if let Some(cwd) = runnable.args.cwd.as_deref() {
                let cwd = Path::new(cwd);
                if cwd != root {
                    text.push_str(&format!(
                        "  (in {})",
                        crate::tools::listing::relative_display(root, cwd)
                    ));
                }
            }
            text.push('\n');
        } else {
            text.push_str(&format!(
                "  ({} runnable; not a cargo command)\n",
                runnable.kind
            ));
        }
    }
    Ok(text)
}

/// Quote an argument for a POSIX shell when it needs it.
fn shell_quote(arg: &str) -> String {
    let plain = !arg.is_empty()
        && arg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_=./:,+@%".contains(c));
    if plain {
        arg.to_string()
    } else {
        format!("'{}'", arg.replace('\'', "'\\''"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expansion_is_the_macro_name_and_its_text() {
        let text = render(
            EXPANDED,
            &json!({ "name": "format!", "expansion": "{\n    let res = ...;\n    res\n}\n" }),
            Path::new("/r"),
            None,
        )
        .unwrap();
        assert_eq!(text, "expanded format\n{\n    let res = ...;\n    res\n}\n");
        assert_eq!(
            render(EXPANDED, &Value::Null, Path::new("/r"), None).unwrap(),
            "not applicable: the focus is not a macro call\n"
        );
    }

    #[test]
    fn related_tests_carry_the_exact_cargo_line() {
        let result = json!([{
            "runnable": {
                "label": "test tests::smoke",
                "kind": "cargo",
                "location": {
                    "targetUri": "file:///r/src/lib.rs",
                    "targetRange": { "start": { "line": 70, "character": 4 }, "end": { "line": 73, "character": 5 } },
                    "targetSelectionRange": { "start": { "line": 71, "character": 7 }, "end": { "line": 71, "character": 12 } }
                },
                "args": {
                    "workspaceRoot": "/r",
                    "cwd": "/r",
                    "cargoArgs": ["test", "--package", "fixture", "--lib"],
                    "executableArgs": ["tests::smoke", "--exact", "--show-output"]
                }
            }
        }]);
        let text = render(RELATED_TESTS, &result, Path::new("/r"), None).unwrap();
        assert_eq!(
            text,
            "1 related test\ntest tests::smoke · src/lib.rs:72\n  cargo test --package fixture --lib -- tests::smoke --exact --show-output\n"
        );
        assert_eq!(
            render(RELATED_TESTS, &json!([]), Path::new("/r"), None).unwrap(),
            "no related tests\n"
        );
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
        assert_eq!(shell_quote("a b"), "'a b'");
    }
}
