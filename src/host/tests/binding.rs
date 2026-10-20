//! A call naming another `workspace`: the same agent bound through the
//! same host, sharing that root's servers, without touching the caller's
//! own binding (`docs/ownership.md`).

use super::fixtures::{alive, fixture};
use super::*;

#[test]
fn a_call_elsewhere_binds_the_same_agent_and_shares_that_roots_servers() {
    let (Some(home), Some(other)) = (fixture("elsewhere-home"), fixture("elsewhere-other")) else {
        return;
    };
    let (home_root, other_root) = (canonical_root(&home.root), canonical_root(&other.root));
    let host = Arc::new(Host::new());
    let session = host.bind(AgentId::new("a"), &home.root);
    session.activate(None);

    let first = session
        .elsewhere(Some(other.root.to_str().unwrap()))
        .unwrap();
    first.activate(None);
    assert_eq!(
        first.view(),
        other_root,
        "the call answers about the other root"
    );
    assert_eq!(
        session.view(),
        home_root,
        "the caller's own view is unchanged"
    );
    let launched = other.launches();
    assert!(!launched.is_empty(), "the first call warmed the other root");

    // A second call, and another agent on the same root, share them.
    let second = session
        .elsewhere(Some(other.root.to_str().unwrap()))
        .unwrap();
    second.activate(None);
    let neighbour = host.agent_started(AgentId::new("b"), &other.root);
    neighbour.activate(None);
    assert_eq!(other.launches(), launched, "one set of servers per root");

    // The agent's interest is recorded on the other root as on its own.
    assert!(
        host.lock()
            .get(&other_root)
            .is_some_and(|entry| entry.interested().contains(&AgentId::new("a")))
    );

    // Ending the agent leaves the neighbour's binding — and the shared
    // servers — in place.
    drop((first, second));
    host.agent_ended(&AgentId::new("a"));
    assert!(launched.iter().copied().all(alive));
    assert!(host.lock().contains_key(&other_root));
    assert!(!host.lock().contains_key(&home_root));
    drop(neighbour);
    host.shutdown();
}

#[test]
fn only_the_first_call_elsewhere_warms_the_whole_root() {
    let (Some(home), Some(other)) = (fixture("rewarm-home"), fixture("rewarm-other")) else {
        return;
    };
    let host = Arc::new(Host::new());
    let session = host.bind(AgentId::new("a"), &home.root);
    let named = other.root.to_str().unwrap();
    let first = session.elsewhere(Some(named)).unwrap();
    first.activate(None);

    // Stop one server, as the idle sweep would. A later call elsewhere
    // does not warm it again — warming on every call would refresh every
    // idle clock in the workspace — so the slot is still cold when the
    // binding returns; the call's own routing starts what it asks for.
    let stopped = {
        let mut workspace = first.workspace();
        let index = workspace.index_of("pyright").expect("pyright is up");
        workspace.stop(index)
    };
    drop(stopped);
    let again = session.elsewhere(Some(named)).unwrap();
    let state = |name: &str| {
        let workspace = again.workspace();
        let slot = workspace
            .slots()
            .iter()
            .find(|slot| slot.name() == name)
            .expect("the fixture names both servers")
            .state();
        slot.label()
    };
    assert_eq!(state("pyright"), "not started");
    again.activate(Some(&other.root.join("src/helper.py")));
    assert_eq!(state("pyright"), "running");
    drop((first, again));
    host.shutdown();
}

#[test]
fn a_workspace_is_a_directory_relative_to_the_view() {
    let Some(home) = fixture("relative") else {
        return;
    };
    let host = Arc::new(Host::new());
    let session = host.bind(AgentId::new("a"), &home.root);

    // A subdirectory of the same project is the same workspace, viewed
    // from the subdirectory; nothing is bound again.
    let root = canonical_root(&home.root);
    let inside = session.elsewhere(Some("src")).unwrap();
    assert_eq!(inside.view(), root.join("src"));
    assert_eq!(host.lock().len(), 1);
    assert_eq!(session.elsewhere(None).unwrap().view(), root);
    assert_eq!(session.elsewhere(Some(" ")).unwrap().view(), root);

    let missing = session.elsewhere(Some("nope")).err().unwrap().to_string();
    assert!(
        missing.starts_with("`workspace` names `nope`, which does not exist"),
        "{missing}"
    );
    let file = session
        .elsewhere(Some("src/lib.rs"))
        .err()
        .unwrap()
        .to_string();
    assert_eq!(
        file,
        "`workspace` names `src/lib.rs`, which is a file, not a directory"
    );
    drop(inside);
    host.shutdown();
}
