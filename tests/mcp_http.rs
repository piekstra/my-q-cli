//! `myq mcp --http` end to end: the real binary on 127.0.0.1, driven with raw HTTP the way the
//! deploy-your-own connector's Worker reaches it through a tunnel. Keychain-free: the bearer token
//! comes from `MYQ_MCP_TOKEN`. No phone can be reached: `MYQ_ADB` names nothing, and a tap moves a
//! door. The rules the server enforces are cli-common's, tested there; this holds myq to them, and
//! to serving the tools that reach the phone only when asked to.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::process::{Child, ChildStderr, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const TOKEN: &str = "5f0c1e2d3b4a69788796a5b4c3d2e1f00f1e2d3c4b5a69788796a5b4c3d2e1f0";
const WRITES: [&str; 3] = ["myq_app_open", "myq_door_open", "myq_door_close"];

fn myq() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_myq"));
    c.env_remove("MYQ_MCP_TOKEN")
        .env_remove("MYQ_ACCOUNT")
        .env("MYQ_ADB", "/nonexistent/adb")
        .env("XDG_CONFIG_HOME", env!("CARGO_TARGET_TMPDIR"))
        .env("HOME", env!("CARGO_TARGET_TMPDIR"))
        .env("USERPROFILE", env!("CARGO_TARGET_TMPDIR"));
    c
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Each line the server writes to stderr, read on a thread until it closes.
fn stderr_lines(stderr: ChildStderr) -> Receiver<String> {
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            let _ = tx.send(line);
        }
    });
    rx
}

struct Server {
    child: Child,
    port: u16,
}

impl Server {
    /// The server on a free port, ready once its own start line names that port. A port taken
    /// between `free_port` and the bind is retried on another.
    fn start(extra: &[&str]) -> Self {
        for _ in 0..3 {
            let port = free_port();
            let mut child = myq()
                .args(["mcp", "--http", "--port", &port.to_string()])
                .args(extra)
                .env("MYQ_MCP_TOKEN", TOKEN)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .expect("myq starts");
            let lines = stderr_lines(child.stderr.take().unwrap());
            let ready = format!("at http://127.0.0.1:{port}/mcp");
            let deadline = Instant::now() + Duration::from_secs(20);
            let mut seen = String::new();
            loop {
                match lines.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                    Ok(line) if line.contains(&ready) => return Self { child, port },
                    Ok(line) => {
                        seen.push_str(&line);
                        seen.push('\n');
                    }
                    Err(RecvTimeoutError::Disconnected) => {
                        let status = child.wait().unwrap();
                        if seen.contains("listening on") {
                            break; // the port was taken: try another
                        }
                        panic!("myq exited ({status}) before serving: {seen}");
                    }
                    Err(RecvTimeoutError::Timeout) => {
                        let _ = child.kill();
                        panic!("myq was not serving on {port} within 20 s: {seen}");
                    }
                }
            }
        }
        panic!("no free port could be bound in three tries");
    }

    /// One POST /mcp: the status and the JSON body.
    fn post(&self, auth: Option<&str>, body: Value) -> (u16, Value) {
        let mut stream = TcpStream::connect(SocketAddr::from(([127, 0, 0, 1], self.port))).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(20)))
            .unwrap();
        let body = body.to_string();
        let mut req = format!(
            "POST /mcp HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\
             Content-Type: application/json\r\nAccept: application/json, text/event-stream\r\n\
             MCP-Protocol-Version: 2025-11-25\r\nContent-Length: {}\r\n",
            body.len()
        );
        if let Some(token) = auth {
            req.push_str(&format!("Authorization: Bearer {token}\r\n"));
        }
        req.push_str("\r\n");
        req.push_str(&body);
        stream.write_all(req.as_bytes()).unwrap();
        let mut raw = String::new();
        stream.read_to_string(&mut raw).unwrap();
        let (head, body) = raw.split_once("\r\n\r\n").expect("a response head");
        let status: u16 = head.split(' ').nth(1).unwrap().parse().unwrap();
        (
            status,
            serde_json::from_str(body.trim()).unwrap_or(Value::Null),
        )
    }

    fn list(&self) -> Vec<String> {
        let (status, got) = self.post(
            Some(TOKEN),
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
        );
        assert_eq!(status, 200, "{got}");
        got["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect()
    }

    fn call(&self, name: &str, arguments: Value) -> Value {
        let (status, got) = self.post(
            Some(TOKEN),
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
                "params": {"name": name, "arguments": arguments}}),
        );
        assert_eq!(status, 200, "{got}");
        got
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn args_for(tool: &str) -> Value {
    if tool == "myq_app_open" {
        json!({})
    } else {
        json!({"door": "garage"})
    }
}

#[test]
fn no_tool_that_reaches_the_phone_is_served_unless_asked_for() {
    let s = Server::start(&[]);
    assert_eq!(s.list(), ["myq_doors"]);
    for tool in WRITES {
        let got = s.call(tool, args_for(tool));
        assert!(got.get("result").is_none(), "{got}");
        let refusal = got["error"]["message"].as_str().unwrap();
        assert!(refusal.contains(tool), "{refusal}");
    }
}

#[test]
fn allow_writes_serves_them_and_a_call_stops_at_the_missing_adb() {
    let s = Server::start(&["--allow-writes"]);
    let mut all = vec!["myq_doors"];
    all.extend(WRITES);
    assert_eq!(s.list(), all);
    for tool in WRITES {
        let got = s.call(tool, args_for(tool));
        assert_eq!(got["result"]["isError"], true, "{got}");
        let text = got["result"]["content"][0]["text"].as_str().unwrap();
        assert!(
            text.contains("did not start") || text.contains("not found"),
            "{text}"
        );
    }
}

#[test]
fn a_request_without_the_token_is_refused() {
    let s = Server::start(&[]);
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"});
    assert_eq!(s.post(None, body.clone()).0, 401);
    assert_eq!(s.post(Some("not-the-token"), body).0, 401);
}

#[test]
fn it_will_not_start_without_a_port_or_a_usable_token() {
    let out = myq().args(["mcp", "--http"]).output().unwrap();
    assert_eq!(out.status.code(), Some(2), "no --port");

    let out = myq()
        .args(["mcp", "--http", "--port", &free_port().to_string()])
        .env("MYQ_MCP_TOKEN", "too-short")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&out.stderr).contains("MYQ_MCP_TOKEN"));
}
