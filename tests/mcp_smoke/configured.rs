//! A server that is not built in, named in `structural.toml`: the generic
//! adapter path end to end, against gopls on a scratch Go module. Gated
//! like the other real-server tests; needs `gopls` on `PATH`.

use mcplease::traits::Tool;
use std::path::PathBuf;

fn scratch_module() -> PathBuf {
    let root = std::env::temp_dir().join(format!("structural-gopls-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("go.mod"),
        "module example.com/scratch\n\ngo 1.21\n",
    )
    .unwrap();
    std::fs::write(
        root.join("greet.go"),
        "package scratch\n\n// Greet says hello.\nfunc Greet(name string) string {\n\treturn \"hello \" + name\n}\n",
    )
    .unwrap();
    std::fs::write(
        root.join("main.go"),
        "package scratch\n\nfunc Run() string {\n\treturn Greet(\"world\")\n}\n",
    )
    .unwrap();
    std::fs::write(
        root.join("greet_test.go"),
        "package scratch\n\nimport \"testing\"\n\nfunc TestGreet(t *testing.T) {\n\tif Greet(\"x\") != \"hello x\" {\n\t\tt.Fatal(\"greet\")\n\t}\n}\n",
    )
    .unwrap();
    std::fs::write(
        root.join("structural.toml"),
        "[servers.gopls]\ncommand = [\"gopls\"]\nlanguages = [\"go\"]\n",
    )
    .unwrap();
    std::fs::canonicalize(root).unwrap()
}

#[test]
#[ignore = "spawns gopls (and the built-ins); run explicitly with -- --ignored"]
fn a_configured_server_resolves_through_the_generic_adapter() {
    let root = scratch_module();
    let mut state = structural::state::State::new(root.clone());
    assert!(
        state
            .servers()
            .iter()
            .any(|server| server.name() == "gopls"),
        "gopls started: unavailable = {:?}",
        state
            .unavailable()
            .iter()
            .map(|u| format!("{}: {}", u.name, u.reason))
            .collect::<Vec<_>>()
    );

    let output = structural::tools::Show {
        symbol: "Greet".to_string(),
        path: None,
    }
    .execute(&mut state, &mcplease::types::RequestContext::default())
    .unwrap_or_else(|error| panic!("show Greet: {error:#}"));
    assert!(
        output.starts_with("function Greet · greet.go:4\n"),
        "{output}"
    );
    // The whole function from gopls's documentSymbol extent; no Go grammar
    // here, so the server's extent stands as-is.
    assert!(
        output.contains("func Greet(name string) string {"),
        "{output}"
    );
    assert!(output.contains("return \"hello \" + name"), "{output}");

    // `uses` through the generic adapter: the call in `Run`, and the test
    // file's call set apart by its `_test.go` name.
    let uses = structural::tools::Uses {
        symbol: "Greet".to_string(),
        path: None,
    }
    .execute(&mut state, &mcplease::types::RequestContext::default())
    .unwrap_or_else(|error| panic!("uses Greet: {error:#}"));
    assert_eq!(
        uses,
        "function Greet · greet.go:4 · 2 references · 2 files\n\
         main.go\n  4 · Run · return Greet(\"world\")\n\
         tests (heuristic) · 1 reference\n\
         greet_test.go\n  6 · TestGreet · if Greet(\"x\") != \"hello x\" {\n",
        "{uses}"
    );

    drop(state);
    let _ = std::fs::remove_dir_all(&root);
}
