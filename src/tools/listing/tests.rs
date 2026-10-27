use super::*;

fn entry(container: Option<&str>, line: u32, signature: &str) -> AmbiguousEntry {
    AmbiguousEntry {
        kind_label: Some("fn"),
        name: "new".to_string(),
        container: container.map(str::to_string),
        server: "rust-analyzer".to_string(),
        location: Location {
            uri: "file:///r/src/reactor.rs".parse().unwrap(),
            range: lsp_types::Range::new(Position::new(line, 7), Position::new(line, 10)),
        },
        signature: Some(signature.to_string()),
    }
}

/// Two same-named methods in one file differ by their container, and the
/// container is the qualifier that picks one.
#[test]
fn an_ambiguous_entry_names_its_container() {
    let entries = [
        entry(Some("Reactor"), 79, "fn() -> io::Result<Self>"),
        entry(Some("ScheduledIo"), 311, "fn() -> Self"),
        entry(None, 400, "fn() -> Self"),
    ];
    assert_eq!(
        render_ambiguous(Path::new("/r"), "new", &entries, Exclusions::default()),
        "ambiguous: 3 definitions of 'new'\n\
         fn new · src/reactor.rs:80 · in Reactor · fn() -> io::Result<Self>\n\
         fn new · src/reactor.rs:312 · in ScheduledIo · fn() -> Self\n\
         fn new · src/reactor.rs:401 · fn() -> Self\n"
    );
}
