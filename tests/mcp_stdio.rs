use std::{path::Path, process::Stdio, time::Duration};

use serde_json::{Value, json};
use tempfile::TempDir;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, ChildStdout, Command},
};

const RESPONSE_TIMEOUT: Duration = Duration::from_secs(15);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

struct TestServer {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl TestServer {
    async fn spawn(mode: &str) -> Self {
        let fake = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/fake-grok");
        Self::spawn_with_binary(&fake, Some(mode), None).await
    }

    async fn spawn_with_binary(
        binary: &Path,
        mode: Option<&str>,
        isolated_home: Option<&Path>,
    ) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_grok-build-search-mcp"));
        command
            .env("GROK_BIN", binary)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        if let Some(mode) = mode {
            command.env("FAKE_GROK_MODE", mode);
        }
        if let Some(home) = isolated_home {
            command.env("HOME", home).env("PATH", home);
        }
        let mut child = command.spawn().expect("spawn MCP server");
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Self {
            child,
            stdin,
            stdout,
        }
    }

    async fn send(&mut self, message: Value) {
        self.stdin
            .write_all(format!("{message}\n").as_bytes())
            .await
            .unwrap();
        self.stdin.flush().await.unwrap();
    }

    async fn response(&mut self, expected_id: u64) -> Value {
        tokio::time::timeout(RESPONSE_TIMEOUT, async {
            loop {
                let mut line = String::new();
                let bytes = self.stdout.read_line(&mut line).await.unwrap();
                assert_ne!(bytes, 0, "MCP server closed before id {expected_id}");
                let value: Value = serde_json::from_str(&line)
                    .unwrap_or_else(|error| panic!("non-JSON stdout line {line:?}: {error}"));
                if value["id"] == expected_id {
                    return value;
                }
            }
        })
        .await
        .expect("MCP response timeout")
    }

    async fn initialize(&mut self) {
        self.send(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": { "name": "integration-test", "version": "0.1.0" }
            }
        }))
        .await;
        let initialized = self.response(1).await;
        assert_eq!(
            initialized["result"]["serverInfo"]["name"],
            "grok-build-search"
        );
        self.send(json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {}
        }))
        .await;
    }

    async fn shutdown(mut self) {
        drop(self.stdin);
        tokio::time::timeout(SHUTDOWN_TIMEOUT, self.child.wait())
            .await
            .expect("server shutdown timeout")
            .expect("server wait");
    }
}

#[tokio::test]
async fn missing_grok_is_reported_by_doctor_after_mcp_initializes() {
    let temp = TempDir::new().unwrap();
    let missing = temp.path().join("missing-grok");
    let mut server = TestServer::spawn_with_binary(&missing, None, Some(temp.path())).await;
    server.initialize().await;
    server
        .send(json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "tools/call",
            "params": { "name": "doctor", "arguments": {} }
        }))
        .await;
    let called = server.response(5).await;

    assert_eq!(called["result"]["isError"], true);
    assert_eq!(called["result"]["structuredContent"]["ok"], false);
    assert_eq!(
        called["result"]["structuredContent"]["error"]["code"],
        "GROK_NOT_FOUND"
    );
    server.shutdown().await;
}

#[tokio::test]
async fn stdio_lists_and_calls_search_with_pure_json_stdout() {
    let mut server = TestServer::spawn("search-success").await;
    server.initialize().await;
    server
        .send(json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {} }))
        .await;
    let listed = server.response(2).await;
    let mut names: Vec<&str> = listed["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    names.sort_unstable();
    assert_eq!(names, ["doctor", "web_fetch", "web_search"]);

    server
        .send(json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "web_search",
                "arguments": { "query": "official Rust website", "response_format": "concise" }
            }
        }))
        .await;
    let called = server.response(3).await;
    assert_eq!(called["result"]["isError"], false);
    assert_eq!(called["result"]["structuredContent"]["ok"], true);
    assert_eq!(called["result"]["structuredContent"]["verified"], true);
    server.shutdown().await;
}

#[tokio::test]
async fn stdio_marks_structured_backend_failure_as_tool_error() {
    let mut server = TestServer::spawn("no-sources").await;
    server.initialize().await;
    server
        .send(json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": {
                "name": "web_search",
                "arguments": { "query": "uncited result" }
            }
        }))
        .await;
    let called = server.response(4).await;

    assert_eq!(called["result"]["isError"], true);
    assert_eq!(called["result"]["structuredContent"]["ok"], false);
    assert_eq!(
        called["result"]["structuredContent"]["error"]["code"],
        "NO_SOURCES"
    );
    server.shutdown().await;
}
