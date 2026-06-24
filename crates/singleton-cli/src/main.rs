use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Stdio};
use std::time::{Duration, Instant};

use clap::{Parser, Subcommand, ValueEnum};
use rmcp::ServiceExt;
use serde_json::json;
use singleton_broker::Broker;
use singleton_copilot::CopilotBackend;
use singleton_core::{AgentBackend, Result, SingletonError};
use singleton_host::LocalHostConnector;
use singleton_mcp::SingletonMcpServer;
use singleton_store::Store;
use singleton_test_support::FakeBackend;
use tokio::io::AsyncWriteExt;
use tokio::net::{UnixListener, UnixStream};

#[derive(Debug, Parser)]
#[command(name = "singleton")]
#[command(about = "Durable MCP broker for background agent sessions")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Serve {
        #[arg(long)]
        database: Option<PathBuf>,
        #[arg(long, value_enum, default_value = "fake")]
        backend: BackendKind,
        #[arg(long)]
        once: bool,
        #[arg(long)]
        stdio: bool,
        #[arg(long)]
        daemon: bool,
        #[arg(long)]
        direct: bool,
    },
    Start {
        #[arg(long)]
        database: Option<PathBuf>,
        #[arg(long, value_enum, default_value = "copilot")]
        backend: BackendKind,
    },
    Status {
        #[arg(long)]
        database: Option<PathBuf>,
    },
    Stop {
        #[arg(long)]
        database: Option<PathBuf>,
    },
    McpConfig {
        #[arg(long, value_enum, default_value = "copilot")]
        backend: BackendKind,
        #[arg(long)]
        database: Option<PathBuf>,
    },
    InstallMcp {
        #[arg(long, value_enum)]
        client: McpClientKind,
        #[arg(long, default_value = "singleton")]
        name: String,
        #[arg(long, value_enum, default_value = "copilot")]
        backend: BackendKind,
        #[arg(long)]
        database: Option<PathBuf>,
        #[arg(long)]
        binary: Option<PathBuf>,
        #[arg(long)]
        dry_run: bool,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum BackendKind {
    Fake,
    Copilot,
}

impl BackendKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Fake => "fake",
            Self::Copilot => "copilot",
        }
    }
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum McpClientKind {
    Copilot,
    Claude,
    Codex,
}

impl McpClientKind {
    fn command_name(self) -> &'static str {
        match self {
            Self::Copilot => "copilot",
            Self::Claude => "claude",
            Self::Codex => "codex",
        }
    }
}

#[derive(Debug, Clone)]
struct StatePaths {
    database: PathBuf,
    socket: PathBuf,
    pid: PathBuf,
}

enum ServeMode {
    Once,
    Foreground,
    DirectStdio,
    Daemon(StatePaths),
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_target(false)
        .init();
    let cli = Cli::parse();
    match cli.command {
        Command::Serve {
            database,
            backend,
            once,
            stdio,
            daemon,
            direct,
        } => serve(database, backend, once, stdio, daemon, direct).await,
        Command::Start { database, backend } => start(database, backend).await,
        Command::Status { database } => status(database).await,
        Command::Stop { database } => stop(database).await,
        Command::McpConfig { backend, database } => mcp_config(backend, database),
        Command::InstallMcp {
            client,
            name,
            backend,
            database,
            binary,
            dry_run,
        } => install_mcp(client, &name, backend, database, binary, dry_run),
    }
}

async fn serve(
    database: Option<PathBuf>,
    backend: BackendKind,
    once: bool,
    stdio: bool,
    daemon: bool,
    direct: bool,
) -> Result<()> {
    let paths = resolve_state_paths(database)?;
    if stdio && !direct && !daemon && !once {
        return proxy_stdio_to_daemon(paths, backend).await;
    }
    if daemon {
        return run_backend(paths.database.clone(), backend, ServeMode::Daemon(paths)).await;
    }
    let mode = if stdio {
        ServeMode::DirectStdio
    } else if once {
        ServeMode::Once
    } else {
        ServeMode::Foreground
    };
    run_backend(paths.database, backend, mode).await
}

async fn start(database: Option<PathBuf>, backend: BackendKind) -> Result<()> {
    let paths = resolve_state_paths(database)?;
    ensure_daemon_running(&paths, backend).await?;
    println!(
        "singletond running: pid={}, socket={}",
        fs::read_to_string(&paths.pid).unwrap_or_default().trim(),
        paths.socket.display()
    );
    Ok(())
}

async fn status(database: Option<PathBuf>) -> Result<()> {
    let paths = resolve_state_paths(database)?;
    let daemon = if daemon_socket_ready(&paths).await {
        "running"
    } else {
        "stopped"
    };
    println!("daemon: {daemon}");
    println!("database: {}", paths.database.display());
    println!("socket: {}", paths.socket.display());
    let store = Store::open(paths.database)?;
    let sessions = store.list_sessions()?;
    println!("sessions: {}", sessions.len());
    for session in sessions {
        println!(
            "{}\t{:?}\t{}",
            session.session_id, session.status, session.title
        );
    }
    Ok(())
}

async fn stop(database: Option<PathBuf>) -> Result<()> {
    let paths = resolve_state_paths(database)?;
    if !paths.pid.exists() {
        remove_stale_socket(&paths)?;
        println!("singletond stopped");
        return Ok(());
    }
    let pid_text = fs::read_to_string(&paths.pid)
        .map_err(|error| SingletonError::Store(format!("read {}: {error}", paths.pid.display())))?;
    let pid = pid_text.trim().parse::<u32>().map_err(|error| {
        SingletonError::InvalidState(format!("invalid daemon pid '{}': {error}", pid_text.trim()))
    })?;
    if process_alive(pid)? {
        signal_process(pid, "TERM")?;
        let deadline = Instant::now() + Duration::from_secs(5);
        while process_alive(pid)? && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        if process_alive(pid)? {
            return Err(SingletonError::InvalidState(format!(
                "daemon pid {pid} did not stop after SIGTERM"
            )));
        }
    }
    cleanup_daemon_files(&paths)?;
    println!("singletond stopped");
    Ok(())
}

fn mcp_config(backend: BackendKind, database: Option<PathBuf>) -> Result<()> {
    let executable = resolve_singleton_binary(None)?;
    let args = singleton_server_args(backend, database);
    let config = json!({
        "mcpServers": {
            "singleton": {
                "command": executable,
                "args": args
            }
        }
    });
    let rendered = serde_json::to_string_pretty(&config)
        .map_err(|error| SingletonError::InvalidState(format!("render MCP config: {error}")))?;
    println!("{rendered}");
    Ok(())
}

fn install_mcp(
    client: McpClientKind,
    name: &str,
    backend: BackendKind,
    database: Option<PathBuf>,
    binary: Option<PathBuf>,
    dry_run: bool,
) -> Result<()> {
    let command = install_mcp_command(client, name, backend, database, binary)?;
    if dry_run {
        println!("{}", command.render_shell());
        return Ok(());
    }
    let status = ProcessCommand::new(&command.program)
        .args(&command.args)
        .status()
        .map_err(|error| {
            SingletonError::InvalidState(format!(
                "run MCP installer '{}': {error}",
                command.render_shell()
            ))
        })?;
    if !status.success() {
        return Err(SingletonError::InvalidState(format!(
            "{} exited with {status}",
            command.program
        )));
    }
    println!(
        "registered MCP server '{name}' with {}",
        client.command_name()
    );
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CommandSpec {
    program: String,
    args: Vec<String>,
}

impl CommandSpec {
    fn render_shell(&self) -> String {
        std::iter::once(self.program.as_str())
            .chain(self.args.iter().map(String::as_str))
            .map(shell_quote)
            .collect::<Vec<_>>()
            .join(" ")
    }
}

fn install_mcp_command(
    client: McpClientKind,
    name: &str,
    backend: BackendKind,
    database: Option<PathBuf>,
    binary: Option<PathBuf>,
) -> Result<CommandSpec> {
    let binary = resolve_singleton_binary(binary)?;
    let mut server_args = vec![binary];
    server_args.extend(singleton_server_args(backend, database));
    let mut args = match client {
        McpClientKind::Copilot => vec!["mcp".into(), "add".into(), name.into(), "--".into()],
        McpClientKind::Claude => vec![
            "mcp".into(),
            "add".into(),
            "--transport".into(),
            "stdio".into(),
            name.into(),
            "--".into(),
        ],
        McpClientKind::Codex => vec!["mcp".into(), "add".into(), name.into(), "--".into()],
    };
    args.extend(server_args);
    Ok(CommandSpec {
        program: client.command_name().into(),
        args,
    })
}

fn resolve_singleton_binary(binary: Option<PathBuf>) -> Result<String> {
    let binary = match binary {
        Some(binary) => binary,
        None => std::env::current_exe().map_err(|error| {
            SingletonError::InvalidState(format!("locate singleton binary: {error}"))
        })?,
    };
    Ok(binary.to_string_lossy().to_string())
}

fn singleton_server_args(backend: BackendKind, database: Option<PathBuf>) -> Vec<String> {
    let mut args = vec![
        "serve".to_string(),
        "--stdio".to_string(),
        "--backend".to_string(),
        backend.as_str().to_string(),
    ];
    if let Some(database) = database {
        args.push("--database".to_string());
        args.push(database.to_string_lossy().to_string());
    }
    args
}

fn shell_quote(value: &str) -> String {
    if !value.is_empty()
        && value.bytes().all(|byte| {
            matches!(
                byte,
                b'A'..=b'Z'
                    | b'a'..=b'z'
                    | b'0'..=b'9'
                    | b'/'
                    | b'.'
                    | b'_'
                    | b'-'
                    | b':'
                    | b'='
                    | b'+'
            )
        })
    {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

async fn run_backend(database: PathBuf, backend: BackendKind, mode: ServeMode) -> Result<()> {
    let store = Store::open(database)?;
    match backend {
        BackendKind::Fake => {
            let broker =
                Broker::new_with_reconnect(store, FakeBackend::new(), LocalHostConnector).await?;
            run_broker(broker, mode).await
        }
        BackendKind::Copilot => {
            let cwd = std::env::current_dir().map_err(|error| {
                SingletonError::InvalidState(format!("read current directory: {error}"))
            })?;
            let backend = CopilotBackend::new(cwd).with_request_store(store.clone());
            let broker = Broker::new_with_reconnect(store, backend, LocalHostConnector).await?;
            run_broker(broker, mode).await
        }
    }
}

async fn run_broker<B>(broker: Broker<B, LocalHostConnector>, mode: ServeMode) -> Result<()>
where
    B: AgentBackend + 'static,
{
    match mode {
        ServeMode::DirectStdio => {
            let server = SingletonMcpServer::new(broker);
            let service = server
                .serve(rmcp::transport::io::stdio())
                .await
                .map_err(|error| {
                    SingletonError::InvalidState(format!("start MCP stdio: {error}"))
                })?;
            service
                .waiting()
                .await
                .map_err(|error| SingletonError::InvalidState(format!("run MCP stdio: {error}")))?;
            Ok(())
        }
        ServeMode::Once => {
            let capabilities = broker.get_capabilities();
            println!(
                "singletond ready: protocol={}, tools={}",
                capabilities.protocol_version,
                capabilities.tools.join(",")
            );
            Ok(())
        }
        ServeMode::Foreground => {
            let capabilities = broker.get_capabilities();
            println!(
                "singletond ready: protocol={}, tools={}",
                capabilities.protocol_version,
                capabilities.tools.join(",")
            );
            std::future::pending::<Result<()>>().await
        }
        ServeMode::Daemon(paths) => run_daemon_server(broker, paths).await,
    }
}

async fn run_daemon_server<B>(
    broker: Broker<B, LocalHostConnector>,
    paths: StatePaths,
) -> Result<()>
where
    B: AgentBackend + 'static,
{
    if daemon_socket_ready(&paths).await {
        return Err(SingletonError::InvalidState(format!(
            "daemon already listening on {}",
            paths.socket.display()
        )));
    }
    remove_stale_socket(&paths)?;
    if let Some(parent) = paths.socket.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            SingletonError::Store(format!(
                "create daemon socket directory {}: {error}",
                parent.display()
            ))
        })?;
    }
    let listener = UnixListener::bind(&paths.socket).map_err(|error| {
        SingletonError::InvalidState(format!("bind {}: {error}", paths.socket.display()))
    })?;
    fs::write(&paths.pid, std::process::id().to_string()).map_err(|error| {
        SingletonError::Store(format!("write {}: {error}", paths.pid.display()))
    })?;
    loop {
        let (stream, _) = listener.accept().await.map_err(|error| {
            SingletonError::InvalidState(format!("accept daemon MCP connection: {error}"))
        })?;
        let server = SingletonMcpServer::new(broker.clone());
        tokio::spawn(async move {
            if let Ok(service) = server.serve(stream).await {
                let _ = service.waiting().await;
            }
        });
    }
}

async fn proxy_stdio_to_daemon(paths: StatePaths, backend: BackendKind) -> Result<()> {
    ensure_daemon_running(&paths, backend).await?;
    let stream = UnixStream::connect(&paths.socket).await.map_err(|error| {
        SingletonError::InvalidState(format!("connect {}: {error}", paths.socket.display()))
    })?;
    let (mut socket_read, mut socket_write) = stream.into_split();
    let mut stdin = tokio::io::stdin();
    let mut stdout = tokio::io::stdout();
    let client_to_daemon = async {
        tokio::io::copy(&mut stdin, &mut socket_write).await?;
        socket_write.shutdown().await
    };
    let daemon_to_client = async {
        tokio::io::copy(&mut socket_read, &mut stdout).await?;
        stdout.shutdown().await
    };
    tokio::try_join!(client_to_daemon, daemon_to_client)
        .map_err(|error| SingletonError::InvalidState(format!("proxy stdio: {error}")))?;
    Ok(())
}

async fn ensure_daemon_running(paths: &StatePaths, backend: BackendKind) -> Result<()> {
    if daemon_socket_ready(paths).await {
        return Ok(());
    }
    remove_stale_socket(paths)?;
    let executable = std::env::current_exe().map_err(|error| {
        SingletonError::InvalidState(format!("locate singleton binary: {error}"))
    })?;
    let database_arg = paths.database.to_string_lossy().to_string();
    let mut child = ProcessCommand::new(executable)
        .args([
            "serve",
            "--daemon",
            "--backend",
            backend.as_str(),
            "--database",
            &database_arg,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| SingletonError::InvalidState(format!("spawn singletond: {error}")))?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if daemon_socket_ready(paths).await {
            return Ok(());
        }
        if let Some(status) = child
            .try_wait()
            .map_err(|error| SingletonError::InvalidState(format!("check daemon child: {error}")))?
        {
            return Err(SingletonError::InvalidState(format!(
                "singletond exited during startup with status {status}"
            )));
        }
        if Instant::now() >= deadline {
            return Err(SingletonError::InvalidState(format!(
                "timed out waiting for daemon socket {}",
                paths.socket.display()
            )));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn daemon_socket_ready(paths: &StatePaths) -> bool {
    tokio::time::timeout(
        Duration::from_millis(250),
        UnixStream::connect(&paths.socket),
    )
    .await
    .is_ok_and(|result| result.is_ok())
}

fn process_alive(pid: u32) -> Result<bool> {
    let status = ProcessCommand::new("kill")
        .args(["-0", &pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|error| SingletonError::InvalidState(format!("check process {pid}: {error}")))?;
    Ok(status.success())
}

fn signal_process(pid: u32, signal: &str) -> Result<()> {
    let status = ProcessCommand::new("kill")
        .args([format!("-{signal}"), pid.to_string()])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map_err(|error| SingletonError::InvalidState(format!("signal process {pid}: {error}")))?;
    if status.success() {
        Ok(())
    } else {
        Err(SingletonError::InvalidState(format!(
            "signal process {pid} exited with {status}"
        )))
    }
}

fn cleanup_daemon_files(paths: &StatePaths) -> Result<()> {
    if paths.pid.exists() {
        fs::remove_file(&paths.pid).map_err(|error| {
            SingletonError::Store(format!("remove {}: {error}", paths.pid.display()))
        })?;
    }
    remove_stale_socket(paths)
}

fn remove_stale_socket(paths: &StatePaths) -> Result<()> {
    if paths.socket.exists() {
        fs::remove_file(&paths.socket).map_err(|error| {
            SingletonError::Store(format!("remove {}: {error}", paths.socket.display()))
        })?;
    }
    Ok(())
}

fn resolve_state_paths(database: Option<PathBuf>) -> Result<StatePaths> {
    let database = resolve_database(database)?;
    let directory = database
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf();
    fs::create_dir_all(&directory).map_err(|error| {
        SingletonError::Store(format!(
            "create singleton state directory {}: {error}",
            directory.display()
        ))
    })?;
    let stem = database
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("singleton");
    let pid = directory.join(format!("{stem}.pid"));
    let candidate_socket = directory.join(format!("{stem}.sock"));
    let socket = if candidate_socket.to_string_lossy().len() < 100 {
        candidate_socket
    } else {
        let mut hasher = DefaultHasher::new();
        database.hash(&mut hasher);
        std::env::temp_dir().join(format!("singleton-{:x}.sock", hasher.finish()))
    };
    Ok(StatePaths {
        database,
        socket,
        pid,
    })
}

fn resolve_database(database: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(database) = database {
        return Ok(database);
    }
    let home = std::env::var_os("HOME").ok_or_else(|| {
        SingletonError::InvalidInput("HOME is not set; pass --database explicitly".to_string())
    })?;
    let dir = PathBuf::from(home).join(".singleton");
    fs::create_dir_all(&dir).map_err(|error| {
        SingletonError::Store(format!(
            "create singleton state directory {}: {error}",
            dir.display()
        ))
    })?;
    Ok(dir.join("singleton.db"))
}

#[cfg(test)]
mod tests {
    use tempfile::NamedTempFile;

    use super::*;

    #[test]
    fn explicit_database_path_is_used() -> Result<()> {
        let file = NamedTempFile::new()
            .map_err(|error| SingletonError::Store(format!("create temp db: {error}")))?;
        let resolved = resolve_database(Some(file.path().to_path_buf()))?;
        assert_eq!(resolved, file.path());
        Ok(())
    }

    #[test]
    fn explicit_database_derives_pid_and_socket_paths() -> Result<()> {
        let file = NamedTempFile::new()
            .map_err(|error| SingletonError::Store(format!("create temp db: {error}")))?;
        let paths = resolve_state_paths(Some(file.path().to_path_buf()))?;
        assert_eq!(paths.database, file.path());
        assert_eq!(
            paths.pid.extension().and_then(|value| value.to_str()),
            Some("pid")
        );
        assert_eq!(
            paths.socket.extension().and_then(|value| value.to_str()),
            Some("sock")
        );
        Ok(())
    }

    #[test]
    fn install_mcp_builds_copilot_command() -> Result<()> {
        let command = install_mcp_command(
            McpClientKind::Copilot,
            "singleton",
            BackendKind::Copilot,
            None,
            Some(PathBuf::from("/usr/local/bin/singleton")),
        )?;
        assert_eq!(command.program, "copilot");
        assert_eq!(
            command.args,
            strings(&[
                "mcp",
                "add",
                "singleton",
                "--",
                "/usr/local/bin/singleton",
                "serve",
                "--stdio",
                "--backend",
                "copilot"
            ])
        );
        Ok(())
    }

    #[test]
    fn install_mcp_builds_claude_command_with_database() -> Result<()> {
        let command = install_mcp_command(
            McpClientKind::Claude,
            "singleton-dev",
            BackendKind::Fake,
            Some(PathBuf::from("/tmp/singleton.db")),
            Some(PathBuf::from("/opt/singleton/bin/singleton")),
        )?;
        assert_eq!(command.program, "claude");
        assert_eq!(
            command.args,
            strings(&[
                "mcp",
                "add",
                "--transport",
                "stdio",
                "singleton-dev",
                "--",
                "/opt/singleton/bin/singleton",
                "serve",
                "--stdio",
                "--backend",
                "fake",
                "--database",
                "/tmp/singleton.db"
            ])
        );
        Ok(())
    }

    #[test]
    fn install_mcp_builds_codex_command() -> Result<()> {
        let command = install_mcp_command(
            McpClientKind::Codex,
            "singleton",
            BackendKind::Copilot,
            None,
            Some(PathBuf::from("singleton")),
        )?;
        assert_eq!(command.program, "codex");
        assert_eq!(
            command.args,
            strings(&[
                "mcp",
                "add",
                "singleton",
                "--",
                "singleton",
                "serve",
                "--stdio",
                "--backend",
                "copilot"
            ])
        );
        Ok(())
    }

    #[test]
    fn command_spec_renders_shell_safe_dry_run() {
        let command = CommandSpec {
            program: "copilot".into(),
            args: vec![
                "mcp".into(),
                "add".into(),
                "singleton dev".into(),
                "--".into(),
                "/Applications/singleton bin/singleton".into(),
                "serve".into(),
            ],
        };
        assert_eq!(
            command.render_shell(),
            "copilot mcp add 'singleton dev' -- '/Applications/singleton bin/singleton' serve"
        );
    }

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| (*value).to_string()).collect()
    }
}
