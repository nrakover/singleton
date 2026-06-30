use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{Receiver, channel};
use std::thread;
use std::time::Duration;

use serde_json::{Value, json};
use tempfile::{NamedTempFile, tempdir};

#[test]
fn cli_start_status_stop_fake_daemon() -> TestResult<()> {
    let db = NamedTempFile::new()?;
    let database = db.path().to_string_lossy().to_string();

    run_singleton(["start", "--backend", "fake", "--database", &database])?;
    let _guard = DaemonGuard::new(database.clone());
    run_singleton(["start", "--backend", "fake", "--database", &database])?;
    let status = run_singleton(["status", "--database", &database])?;
    assert!(status.contains("daemon: running"), "{status}");
    assert!(status.contains("pid:"), "{status}");
    assert!(status.contains("(listening)"), "{status}");
    let pid = read_pid(&pid_path(db.path()))?;
    let pgid = process_group_id(pid)?;
    assert_eq!(pgid, pid, "daemon pid {pid} should own its process group");
    run_singleton(["stop", "--database", &database])?;
    run_singleton(["stop", "--database", &database])?;
    let status = run_singleton(["status", "--database", &database])?;
    assert!(status.contains("daemon: stopped"), "{status}");
    Ok(())
}

#[test]
fn cli_status_reports_and_stop_cleans_stale_lifecycle_files() -> TestResult<()> {
    let dir = tempdir()?;
    let database_path = dir.path().join("state.db");
    let database = database_path.to_string_lossy().to_string();
    let pid = pid_path(&database_path);
    let socket = socket_path(&database_path);

    fs::write(&pid, "999999999")?;
    let status = run_singleton(["status", "--database", &database])?;
    assert!(status.contains("daemon: stale pid"), "{status}");
    assert!(
        status.contains("cleanup: singleton stop --database"),
        "{status}"
    );
    run_singleton(["stop", "--database", &database])?;
    assert!(!pid.exists(), "stale pid file should be removed");

    fs::write(&socket, "not a socket")?;
    let status = run_singleton(["status", "--database", &database])?;
    assert!(status.contains("daemon: stale socket"), "{status}");
    assert!(
        status.contains("cleanup: singleton stop --database"),
        "{status}"
    );
    run_singleton(["stop", "--database", &database])?;
    assert!(!socket.exists(), "stale socket file should be removed");

    let status = run_singleton(["status", "--database", &database])?;
    assert!(status.contains("daemon: stopped"), "{status}");
    Ok(())
}

#[test]
fn cli_concurrent_fake_daemon_starts_are_idempotent() -> TestResult<()> {
    let db = NamedTempFile::new()?;
    let database = db.path().to_string_lossy().to_string();
    let handles = (0..6)
        .map(|_| {
            let database = database.clone();
            thread::spawn(move || {
                run_singleton_thread(["start", "--backend", "fake", "--database", &database])
            })
        })
        .collect::<Vec<_>>();

    for handle in handles {
        let result = handle.join().map_err(|_| err("start thread panicked"))?;
        result.map_err(err)?;
    }

    let _guard = DaemonGuard::new(database.clone());
    let status = run_singleton(["status", "--database", &database])?;
    assert!(status.contains("daemon: running"), "{status}");
    run_singleton(["stop", "--database", &database])?;
    Ok(())
}

#[test]
fn cli_mcp_config_uses_effective_config_and_explicit_overrides() -> TestResult<()> {
    let dir = tempdir()?;
    let configured_database = dir.path().join("configured.db");
    let config_path = dir.path().join("singleton.toml");
    fs::write(
        &config_path,
        format!(
            r#"
version = 1
[profiles.default]
backend = "fake"
database = "{}"
"#,
            configured_database.display()
        ),
    )?;
    let config = config_path.to_string_lossy().to_string();

    let output = run_singleton_slice(&["--config", &config, "--no-project-config", "mcp-config"])?;
    let value: Value = serde_json::from_str(&output)?;
    let args = mcp_args(&value)?;
    assert_eq!(arg_value(args, "--backend")?, "fake");
    assert_eq!(arg_value(args, "--config")?, config);
    assert!(has_arg(args, "--no-project-config"));
    assert_eq!(
        arg_value(args, "--database")?,
        configured_database.to_string_lossy().to_string()
    );

    let output = run_singleton_slice(&[
        "--config",
        &config,
        "--no-project-config",
        "mcp-config",
        "--backend",
        "copilot",
    ])?;
    let value: Value = serde_json::from_str(&output)?;
    let args = mcp_args(&value)?;
    assert_eq!(arg_value(args, "--backend")?, "copilot");
    assert_eq!(
        arg_value(args, "--database")?,
        configured_database.to_string_lossy().to_string()
    );
    Ok(())
}

#[test]
fn cli_status_reports_configured_ssh_hosts_without_probe() -> TestResult<()> {
    let dir = tempdir()?;
    let database = dir.path().join("configured.db");
    let config_path = dir.path().join("singleton.toml");
    fs::write(
        &config_path,
        format!(
            r#"
version = 1

[profiles.default]
backend = "fake"
database = "{}"
default_host = "devbox"

[hosts.devbox]
kind = "ssh"
target = "devbox"
"#,
            database.display()
        ),
    )?;
    let config = config_path.to_string_lossy().to_string();

    let status = run_singleton_slice(&["--config", &config, "--no-project-config", "status"])?;

    assert!(status.contains("ssh_hosts: 1"), "{status}");
    assert!(
        status.contains("devbox\tstate=NotChecked\ttarget=devbox"),
        "{status}"
    );
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
    assert!(tool_names.contains(&"get_latest_output"), "{tools}");

    let capabilities = client.call_tool(3, "get_capabilities", json!({}))?;
    assert_eq!(capabilities["protocol_version"], "0.1");
    assert_eq!(capabilities["default_profile"], "default");
    assert_eq!(capabilities["defaults"]["backend"], "fake");
    assert_eq!(capabilities["defaults"]["mode"], "interactive");
    assert_eq!(capabilities["defaults"]["default_host"], "host_local");
    assert_eq!(
        capabilities["defaults"]["repo_workspace_provider"],
        "git_worktree"
    );
    assert_eq!(capabilities["defaults"]["permissions"]["default"], "ask");

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
    let latest = client.call_tool(
        7,
        "get_latest_output",
        json!({
            "session_id": created["session_id"],
            "turn_id": sent["turn_id"]
        }),
    )?;
    assert_eq!(latest["result_text"], "fake turn completed");
    assert_eq!(latest["result_source"], "turn_summary");
    assert_eq!(latest["needs_event_inspection"], false);
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

struct DaemonGuard {
    database: String,
}

impl DaemonGuard {
    fn new(database: String) -> Self {
        Self { database }
    }
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = Command::new(env!("CARGO_BIN_EXE_singleton"))
            .args(["stop", "--database", &self.database])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}

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

fn mcp_args(value: &Value) -> TestResult<&[Value]> {
    value["mcpServers"]["singleton"]["args"]
        .as_array()
        .map(Vec::as_slice)
        .ok_or_else(|| err(format!("missing singleton MCP args: {value}")))
}

fn arg_value(args: &[Value], flag: &str) -> TestResult<String> {
    args.windows(2)
        .find_map(|window| {
            if window.first().and_then(Value::as_str) == Some(flag) {
                window
                    .get(1)
                    .and_then(Value::as_str)
                    .map(ToString::to_string)
            } else {
                None
            }
        })
        .ok_or_else(|| err(format!("missing {flag} in args: {args:?}")))
}

fn has_arg(args: &[Value], flag: &str) -> bool {
    args.iter().any(|arg| arg.as_str() == Some(flag))
}

fn run_singleton<const N: usize>(args: [&str; N]) -> TestResult<String> {
    run_singleton_slice(&args)
}

fn run_singleton_slice(args: &[&str]) -> TestResult<String> {
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

fn run_singleton_thread<const N: usize>(args: [&str; N]) -> Result<String, String> {
    let output = Command::new(env!("CARGO_BIN_EXE_singleton"))
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|error| error.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "singleton command failed with {}: stdout={} stderr={}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    String::from_utf8(output.stdout).map_err(|error| error.to_string())
}

fn pid_path(database: &Path) -> PathBuf {
    state_path(database, "pid")
}

fn socket_path(database: &Path) -> PathBuf {
    state_path(database, "sock")
}

fn state_path(database: &Path, extension: &str) -> PathBuf {
    let directory = database.parent().unwrap_or_else(|| Path::new("."));
    let stem = database
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("singleton");
    directory.join(format!("{stem}.{extension}"))
}

fn read_pid(path: &Path) -> TestResult<u32> {
    Ok(fs::read_to_string(path)?.trim().parse()?)
}

fn process_group_id(pid: u32) -> TestResult<u32> {
    let output = Command::new("ps")
        .args(["-o", "pgid=", "-p", &pid.to_string()])
        .stdin(Stdio::null())
        .output()?;
    if !output.status.success() {
        return Err(err(format!(
            "ps failed with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    Ok(String::from_utf8(output.stdout)?.trim().parse()?)
}
