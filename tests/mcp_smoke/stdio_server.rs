//! A child driven over stdio; every response/event/exit wait is bounded.

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

/// How long any one response, event or exit may take before the test
/// fails rather than hanging.
///
/// A backstop, not a measurement: nothing here asserts that structural is
/// fast, only that it answers. It has to cover a cold rust-analyzer
/// indexing the fixture while the rest of the suite runs — these cases
/// spawn a real child, and the machine is often shared with other agent
/// sessions (`process/testing.md`). At fifteen seconds they failed under
/// the default thread count and passed with `--test-threads=1`, which is
/// a fact about the deadline rather than about the server.
const WAIT: Duration = Duration::from_secs(120);

pub struct Server {
    pub child: Child,
    stdin: Option<std::process::ChildStdin>,
    responses: Receiver<Value>,
    events: Receiver<String>,
    pub next_id: u64,
}

impl Server {
    pub fn start(root: &str) -> Self {
        Self::start_with_env(root, &[])
    }

    pub fn start_with_env(root: &str, env: &[(&str, String)]) -> Self {
        Self::start_args(root, env, &["serve"])
    }

    pub fn start_args(root: &str, env: &[(&str, String)], args: &[&str]) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_structural"));
        command.args(args).env("STRUCTURAL_ROOT", root);
        for (name, value) in env {
            command.env(name, value);
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawning structural");
        let stdin = child.stdin.take();
        let stdout = child.stdout.take().unwrap();
        let stderr = child.stderr.take().unwrap();
        let (send, responses) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines() {
                let Ok(line) = line else {
                    break;
                };
                // CLI output is plain text, not an MCP frame.
                if let Ok(value) = serde_json::from_str(&line)
                    && send.send(value).is_err()
                {
                    break;
                }
            }
        });
        let (send, events) = mpsc::channel();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines() {
                let Ok(line) = line else {
                    break;
                };
                eprintln!("[structural] {line}");
                if send.send(line).is_err() {
                    break;
                }
            }
        });
        Self {
            child,
            stdin,
            responses,
            events,
            next_id: 0,
        }
    }

    pub fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.send(method, params);
        self.wait_for(id)
    }

    pub fn send(&mut self, method: &str, params: Value) -> u64 {
        self.next_id += 1;
        let id = self.next_id;
        let frame = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        let stdin = self.stdin.as_mut().expect("stdin not closed");
        writeln!(stdin, "{frame}").expect("writing request");
        stdin.flush().unwrap();
        id
    }

    pub fn next_response(&mut self) -> Value {
        self.responses
            .recv_timeout(WAIT)
            .expect("response within deadline")
    }

    pub fn notify(&mut self, method: &str, params: Value) {
        let frame = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        let stdin = self.stdin.as_mut().unwrap();
        writeln!(stdin, "{frame}").unwrap();
        stdin.flush().unwrap();
    }

    pub fn close_stdin(&mut self) {
        self.stdin.take();
    }

    pub fn wait_for(&mut self, id: u64) -> Value {
        let deadline = Instant::now() + WAIT;
        loop {
            let value = self
                .responses
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .expect("response within deadline");
            if value.get("id").and_then(Value::as_u64) == Some(id) {
                return value;
            }
        }
    }

    pub fn event(&self, needle: &str) -> String {
        let deadline = Instant::now() + WAIT;
        loop {
            let line = self
                .events
                .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                .expect("stderr event within deadline");
            if line.contains(needle) {
                return line;
            }
        }
    }

    pub fn wait_exit(&mut self) -> std::process::ExitStatus {
        let deadline = Instant::now() + WAIT;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "structural did not exit within deadline"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.close_stdin();
        // Let EOF reclaim owned servers before resorting to SIGKILL. No
        // unbounded reads or waits while unwinding a failed assertion.
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) | Err(_) => return,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10))
                }
                Ok(None) => break,
            }
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
