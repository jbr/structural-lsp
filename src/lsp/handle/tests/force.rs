//! Force uses protocol/socket barriers, not sleeps, to choose interleavings.
#![cfg(unix)]

use super::*;
use crate::test_support::reaped;
use std::io::Read;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;

struct Peers {
    listener: UnixListener,
    path: PathBuf,
}

impl Peers {
    fn new(name: &str) -> Self {
        // macOS SUN_LEN is 104; the temp dir is deep, so keep the name short.
        let path = std::env::temp_dir().join(format!("s{}/f-{name}", std::process::id()));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let listener = UnixListener::bind(&path).unwrap();
        Self { listener, path }
    }

    fn accept(&self) -> (UnixStream, i32) {
        let (mut stream, _) = self.listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut pid = [0; 4];
        stream.read_exact(&mut pid).unwrap();
        (stream, i32::from_be_bytes(pid))
    }
}

impl Drop for Peers {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

fn byte(stream: &mut UnixStream, expected: u8) {
    let mut got = [0];
    stream.read_exact(&mut got).unwrap();
    assert_eq!(got[0], expected);
}

fn closed(stream: &mut UnixStream) {
    assert_eq!(
        stream.read(&mut [0]).unwrap(),
        0,
        "peer still has an open descriptor"
    );
}

fn force_mock(name: &str, path: &std::path::Path) -> Mock {
    let mock = Mock::new(name);
    let script = MOCK_PY
        .replace(
            "import json, sys, threading, time",
            &format!(
                r#"
import json, sys, threading, time, os, socket, subprocess, struct
peer = socket.socket(socket.AF_UNIX)
peer.connect({path:?})
peer.sendall(struct.pack('!i', os.getpid()))
"#,
                path = path.to_string_lossy()
            ),
        )
        .replace(
            "        elif method == \"test/never\":\n            pass",
            r#"
        elif method == "test/never":
            peer.sendall(b'N')
        elif method == "$/structural/ping":
            reply(msg, "alive")
            peer.sendall(b'P')
        elif method == "test/descendant":
            code = '''
import os, socket, struct, sys
s = socket.socket(socket.AF_UNIX)
s.connect(sys.argv[1])
s.sendall(struct.pack('!i', os.getpid()))
s.recv(1)
'''
            subprocess.Popen([sys.executable, '-c', code, params['path']])
            reply(msg, 'spawned')
        elif method == "test/leader-exit":
            # Deliberately leave stdout inherited by the descendant; no EOF
            # reaches the transport even after this leader exits naturally.
            reply(msg, 'exiting')
            sys.exit(0)
"#,
        )
        .replace(
            "        elif method == \"shutdown\":\n            reply(msg, None)",
            "        elif method == \"shutdown\":\n            peer.sendall(b'S')",
        );
    std::fs::write(&mock.script, script).unwrap();
    mock
}

#[test]
fn force_interrupts_probe_answering_query_and_graceful_wait() {
    needs_python!();
    let peers = Peers::new("query");
    let mock = force_mock("force-query", &peers.path);
    let mut settings = test_settings();
    settings.probe_after = Duration::from_millis(30);
    settings.shutdown_grace = Duration::from_secs(60);
    let handle = mock.spawn(settings, Box::new(StandardIdle));
    let (mut peer, pid) = peers.accept();
    initialize(&handle);
    let caller = handle.clone_for_test();
    let query = thread::spawn(move || caller.request("test/never", Value::Null));
    byte(&mut peer, b'N');
    byte(&mut peer, b'P');
    let stopper = handle.stopper();
    stopper.stop();
    // Probes already in flight may precede the shutdown request.
    loop {
        let mut got = [0];
        peer.read_exact(&mut got).unwrap();
        if got[0] == b'S' {
            break;
        }
        assert_eq!(got[0], b'P');
    }
    let shutdown = thread::spawn(move || handle.shutdown().unwrap());
    let started = Instant::now();
    stopper.force();
    assert!(started.elapsed() < Duration::from_secs(1));
    assert!(
        query
            .join()
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("forced shutdown")
    );
    closed(&mut peer);
    shutdown.join().unwrap();
    reaped(pid);
    // Retaining and invoking stop capabilities after reap cannot signal a
    // reused PID: they contain only flags and a channel.
    stopper.force();
    stopper.stop();
}

#[test]
fn force_kills_descendants_with_live_or_exited_leader() {
    needs_python!();
    for exit_first in [false, true] {
        let name = if exit_first { "exited" } else { "live" };
        let peers = Peers::new(name);
        let mock = force_mock(name, &peers.path);
        let handle = mock.spawn(test_settings(), Box::new(StandardIdle));
        let (mut leader, pid) = peers.accept();
        initialize(&handle);
        handle
            .request("test/descendant", json!({"path": peers.path}))
            .unwrap();
        let (mut descendant, _) = peers.accept();
        if exit_first {
            handle.request("test/leader-exit", Value::Null).unwrap();
            closed(&mut leader);
        }
        handle.stopper().force();
        handle.shutdown().unwrap();
        if !exit_first {
            closed(&mut leader);
        }
        closed(&mut descendant);
        reaped(pid);
    }
}

#[test]
fn concurrent_force_and_stop_interrupt_initialization() {
    needs_python!();
    let peers = Peers::new("init");
    let mock = force_mock("force-init", &peers.path);
    let script = std::fs::read_to_string(&mock.script)
        .unwrap()
        .replace("reply(msg, {\"capabilities\": {}})", "peer.sendall(b'I')");
    std::fs::write(&mock.script, script).unwrap();
    let mut settings = test_settings();
    settings.shutdown_grace = Duration::from_secs(60);
    let handle = mock.spawn(settings, Box::new(StandardIdle));
    let stopper = handle.stopper();
    let (mut peer, pid) = peers.accept();
    let caller = handle.clone_for_test();
    let initialize = thread::spawn(move || caller.request("initialize", Value::Null));
    byte(&mut peer, b'I');
    thread::scope(|scope| {
        for _ in 0..8 {
            scope.spawn(|| {
                stopper.stop();
                stopper.force();
                stopper.stop();
            });
        }
    });
    assert!(
        initialize
            .join()
            .unwrap()
            .unwrap_err()
            .to_string()
            .contains("forced shutdown")
    );
    handle.shutdown().unwrap();
    reaped(pid);
}

#[test]
fn every_thread_start_failure_reclaims_child() {
    needs_python!();
    for stage in ["lsp-reader", "lsp-writer", "lsp-owner"] {
        let peers = Peers::new(stage);
        let mock = force_mock(stage, &peers.path);
        let mut peer = None;
        let error = startup::spawn(
            &mock.python,
            &[mock.script.to_string_lossy().into_owned()],
            test_settings(),
            Box::new(StandardIdle),
            |name, task| {
                if name == stage {
                    // Force startup far enough that the child is alive,
                    // then simulate Builder::spawn dropping its closure.
                    peer = Some(peers.accept());
                    drop(task);
                    return Err(std::io::Error::other("injected thread start failure"));
                }
                thread::Builder::new().name(name.into()).spawn(task)
            },
        )
        .err()
        .expect("injected failure");
        assert!(error.to_string().contains("spawning"));
        let (mut peer, pid) = peer.unwrap();
        closed(&mut peer);
        reaped(pid);
    }
}

#[test]
fn spawn_failure_is_reported() {
    assert!(ServerHandle::spawn("/nonexistent/structural-force-mock", &[]).is_err());
}
