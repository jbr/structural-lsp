//! A `structural serve` child driven over stdio, for the MCP round trips.

use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdout, Command, Stdio};

pub struct Server {
    pub child: Child,
    pub stdin: std::process::ChildStdin,
    pub stdout: BufReader<ChildStdout>,
    pub next_id: u64,
}

impl Server {
    pub fn start(root: &str) -> Self {
        Self::start_with_env(root, &[])
    }

    /// Start with extra environment variables, e.g. a `PATH` that puts a
    /// scripted `rust-analyzer` first.
    pub fn start_with_env(root: &str, env: &[(&str, String)]) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_structural"));
        command.arg("serve").env("STRUCTURAL_ROOT", root);
        for (name, value) in env {
            command.env(name, value);
        }
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawning `structural serve`");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Self {
            child,
            stdin,
            stdout,
            next_id: 0,
        }
    }

    pub fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.send(method, params);
        self.wait_for(id)
    }

    /// Write a request and return its id without waiting for the response.
    pub fn send(&mut self, method: &str, params: Value) -> u64 {
        self.next_id += 1;
        let id = self.next_id;
        let frame = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        writeln!(self.stdin, "{frame}").expect("writing the request");
        self.stdin.flush().unwrap();
        id
    }

    /// The next response line, whatever its id.
    pub fn next_response(&mut self) -> Value {
        let mut line = String::new();
        let bytes = self
            .stdout
            .read_line(&mut line)
            .expect("reading the response line");
        assert!(bytes > 0, "server closed stdout");
        serde_json::from_str(&line).expect("a JSON line")
    }

    pub fn notify(&mut self, method: &str, params: Value) {
        let frame = json!({ "jsonrpc": "2.0", "method": method, "params": params });
        writeln!(self.stdin, "{frame}").unwrap();
        self.stdin.flush().unwrap();
    }

    /// Read lines until the response with `id` arrives.
    pub fn wait_for(&mut self, id: u64) -> Value {
        let mut line = String::new();
        loop {
            line.clear();
            let bytes = self
                .stdout
                .read_line(&mut line)
                .expect("reading the response line");
            assert!(
                bytes > 0,
                "server closed stdout before responding to id {id}"
            );
            let value: Value = serde_json::from_str(&line).expect("a JSON line");
            if value.get("id").and_then(Value::as_u64) == Some(id) {
                return value;
            }
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // Best-effort polite exit so rust-analyzer does not log a shutdown
        // complaint; the kill remains the backstop.
        let _ = writeln!(self.stdin, r#"{{"jsonrpc":"2.0","method":"exit"}}"#);
        let _ = self.stdin.flush();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
