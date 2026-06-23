use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};
use tempfile::NamedTempFile;

#[test]
fn cli_start_status_stop_fake_daemon() -> TestResult<()> {
    let db = NamedTempFile::new()?;
    let database = db.path().to_string_lossy().to_string();

    run_singleton(["start", "--backend", "fake", "--database", &database])?;
    let status = run_singleton(["status", "--database", &database])?;
    assert!(status.contains("daemon: running"), "{status}");
    run_singleton(["stop", "--database", &database])?;
    let status = run_singleton(["status", "--database", &database])?;
    assert!(status.contains("daemon: stopped"), "{status}");
    Ok(())
}

#[test]
fn stdio_mcp_serves_fake_backend_vertical_slice() -> TestResult<()> {
    let db = NamedTempFile::new()?;
    let mut client = StdioMcpClient::spawn("fake", db.path().to_string_lossy().as_ref())?;

    client.initialize()?;

    let tools = client.request(json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list",
        "params": {}
    }))?;
    let tool_names = tools["result"]["tools"]
        .as_array()
        .ok_or_else(|| err("tools/list result did not contain a tools array"))?
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect::<Vec<_>>();
    assert!(tool_names.contains(&"create_session"), "{tools}");
    assert!(tool_names.contains(&"send_message"), "{tools}");
    assert!(tool_names.contains(&"read_events"), "{tools}");

    let capabilities = client.call_tool(3, "get_capabilities", json!({}))?;
    assert_eq!(capabilities["protocol_version"], "0.1");

    let created = client.call_tool(
        4,
        "create_session",
        json!({
            "description": "stdio MCP vertical slice"
        }),
    )?;
    let session_id = created["session_id"]
        .as_str()
        .ok_or_else(|| err("create_session result did not contain session_id"))?
        .to_string();

    let sent = client.call_tool(
        5,
        "send_message",
        json!({
            "session_id": session_id,
            "message": "hello from stdio"
        }),
    )?;
    assert_eq!(sent["status"], "running");

    let events = client.call_tool(
        6,
        "read_events",
        json!({
            "session_id": created["session_id"],
            "cursor": sent["event_cursor"],
            "limit": 100,
            "event_types": ["turn.completed"],
            "wait_ms": 1000
        }),
    )?;
    assert!(
        events["events"]
            .as_array()
            .ok_or_else(|| err("read_events result did not contain events array"))?
            .iter()
            .any(|event| event["event_type"] == "turn.completed"),
        "{events}"
    );
    Ok(())
}

#[cfg(feature = "live-copilot")]
#[test]
#[ignore = "requires authenticated GitHub Copilot CLI access"]
fn live_stdio_mcp_serves_copilot_backend() -> TestResult<()> {
    let db = NamedTempFile::new()?;
    let mut client = StdioMcpClient::spawn("copilot", db.path().to_string_lossy().as_ref())?;
    client.initialize()?;

    let created = client.call_tool(
        10,
        "create_session",
        json!({
            "description": "live stdio MCP smoke"
        }),
    )?;
    let session_id = created["session_id"]
        .as_str()
        .ok_or_else(|| err("create_session result did not contain session_id"))?
        .to_string();

    let sent = client.call_tool(
        11,
        "send_message",
        json!({
            "session_id": session_id,
            "message": "Reply with exactly: singleton live stdio ok"
        }),
    )?;
    assert_eq!(sent["status"], "running");
    client.wait_for_event(
        12,
        created["session_id"].clone(),
        sent["event_cursor"].as_i64().unwrap_or_default(),
        "turn.completed",
        4,
    )?;
    Ok(())
}

type TestResult<T> = Result<T, Box<dyn std::error::Error>>;

struct StdioMcpClient {
    child: Child,
    stdin: ChildStdin,
    rx: Receiver<Value>,
    database: String,
}

impl StdioMcpClient {
    fn spawn(backend: &str, database: &str) -> TestResult<Self> {
        let mut child = Command::new(env!("CARGO_BIN_EXE_singleton"))
            .args([
                "serve",
                "--backend",
                backend,
                "--database",
                database,
                "--stdio",
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| err("singleton process did not expose stdout"))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| err("singleton process did not expose stdin"))?;
        let (tx, rx) = channel();
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            while reader.read_line(&mut line).unwrap_or(0) > 0 {
                let trimmed = line.trim();
                if !trimmed.is_empty()
                    && let Ok(value) = serde_json::from_str::<Value>(trimmed)
                {
                    let _ = tx.send(value);
                }
                line.clear();
            }
        });
        Ok(Self {
            child,
            stdin,
            rx,
            database: database.to_string(),
        })
    }

    fn initialize(&mut self) -> TestResult<()> {
        let initialized = self.request(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": {
                    "name": "singleton-stdio-test",
                    "version": "0.1.0"
                }
            }
        }))?;
        if initialized.get("result").is_none() {
            return Err(err(format!("initialize failed: {initialized}")));
        }
        self.notify(json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {}
        }))
    }

    fn notify(&mut self, value: Value) -> TestResult<()> {
        writeln!(self.stdin, "{value}")?;
        self.stdin.flush()?;
        Ok(())
    }

    fn request(&mut self, value: Value) -> TestResult<Value> {
        let id = value["id"].clone();
        writeln!(self.stdin, "{value}")?;
        self.stdin.flush()?;
        loop {
            let response = self.rx.recv_timeout(Duration::from_secs(5))?;
            if response.get("id") == Some(&id) {
                if response.get("error").is_some() {
                    return Err(err(format!("MCP error response: {response}")));
                }
                return Ok(response);
            }
        }
    }

    fn call_tool(&mut self, id: i64, name: &str, arguments: Value) -> TestResult<Value> {
        let response = self.request(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {
                "name": name,
                "arguments": arguments
            }
        }))?;
        tool_payload(&response["result"])
    }

    #[cfg(feature = "live-copilot")]
    fn wait_for_event(
        &mut self,
        starting_id: i64,
        session_id: Value,
        mut cursor: i64,
        event_type: &str,
        attempts: usize,
    ) -> TestResult<Value> {
        for offset in 0..attempts {
            let events = self.call_tool(
                starting_id + i64::try_from(offset)?,
                "read_events",
                json!({
                    "session_id": session_id,
                    "cursor": cursor,
                    "limit": 100,
                    "event_types": [event_type],
                    "wait_ms": 30000
                }),
            )?;
            if let Some(event) = events["events"].as_array().and_then(|events| {
                events
                    .iter()
                    .find(|event| event["event_type"] == event_type)
            }) {
                return Ok(event.clone());
            }
            cursor = events["next_cursor"].as_i64().unwrap_or(cursor);
        }
        Err(err(format!("timed out waiting for {event_type}")))
    }
}

impl Drop for StdioMcpClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = Command::new(env!("CARGO_BIN_EXE_singleton"))
            .args(["stop", "--database", &self.database])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

fn tool_payload(result: &Value) -> TestResult<Value> {
    if let Some(payload) = result.get("structuredContent") {
        return Ok(payload.clone());
    }
    if let Some(text) = result["content"]
        .as_array()
        .and_then(|content| content.first())
        .and_then(|item| item["text"].as_str())
    {
        return Ok(serde_json::from_str(text)?);
    }
    Err(err(format!("missing structured tool payload: {result}")))
}

fn err(message: impl Into<String>) -> Box<dyn std::error::Error> {
    std::io::Error::other(message.into()).into()
}

fn run_singleton<const N: usize>(args: [&str; N]) -> TestResult<String> {
    let output = Command::new(env!("CARGO_BIN_EXE_singleton"))
        .args(args)
        .stdin(Stdio::null())
        .output()?;
    if !output.status.success() {
        return Err(err(format!(
            "singleton command failed with {}: stdout={} stderr={}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(String::from_utf8(output.stdout)?)
}
