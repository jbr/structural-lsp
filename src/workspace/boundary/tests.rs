use super::*;

#[test]
fn a_drain_reduces_to_each_paths_state_now_outside_skipped_directories() {
    let root = std::env::temp_dir().join(format!("structural-boundary-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::write(root.join("src/kept.rs"), "").unwrap();
    let path = |relative: &str| root.join(relative);
    let events = vec![
        Event::Created {
            path: path("src/kept.rs"),
        },
        // A server's own build output, and tool state.
        Event::Changed {
            path: path("target/debug/build/output"),
        },
        Event::Created {
            path: path(".git/index"),
        },
        // Created and gone by the drain: the disk says deleted.
        Event::Created {
            path: path("src/gone.rs"),
        },
        Event::Changed {
            path: path("src/kept.rs"),
        },
        Event::Overflow,
    ];
    assert_eq!(
        reduce_watched(&root, &events),
        vec![
            (path("src/kept.rs"), FileChangeKind::Changed),
            (path("src/gone.rs"), FileChangeKind::Deleted),
        ]
    );
    let _ = std::fs::remove_dir_all(&root);
}
