use std::collections::hash_map::DefaultHasher;
use std::fs::{self, File};
use std::hash::{Hash, Hasher};
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use clap::{Parser, Subcommand, ValueEnum};
use fs2::FileExt;
use rmcp::ServiceExt;
use serde_json::json;
use singleton_broker::Broker;
use singleton_config::{
    ConfigEnvironment, ConfigFieldOverrides, ConfigLoadOptions, EffectiveConfig,
    EffectiveHostConfig, default_database_path, load_effective_config,
};
use singleton_copilot::CopilotBackend;
use singleton_core::{AgentBackend, RemoteBrokerRegistry, Result, SingletonError};
use singleton_host::LocalHostConnector;
use singleton_mcp::SingletonMcpServer;
use singleton_remote::SshRemoteBrokerRegistry;
use singleton_store::Store;
use singleton_test_support::FakeBackend;
use tokio::io::AsyncWriteExt;
use tokio::net::{UnixListener, UnixStream};

mod update;

const DAEMON_STARTUP_LOCK_HELD_ENV: &str = "SINGLETON_DAEMON_STARTUP_LOCK_HELD";

#[derive(Debug, Parser)]
#[command(name = "singleton")]
#[command(about = "Durable MCP broker for background agent sessions")]
#[command(version)]
struct Cli {
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[arg(long, global = true)]
    profile: Option<String>,
    #[arg(long, global = true)]
    no_project_config: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Serve {
        #[arg(long)]
        database: Option<PathBuf>,
        #[arg(long, value_enum)]
        backend: Option<BackendKind>,
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
        #[arg(long, value_enum)]
        backend: Option<BackendKind>,
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
        #[arg(long, value_enum)]
        backend: Option<BackendKind>,
        #[arg(long)]
        database: Option<PathBuf>,
    },
    InstallMcp {
        #[arg(long, value_enum)]
        client: McpClientKind,
        #[arg(long, default_value = "singleton")]
        name: String,
        #[arg(long, value_enum)]
        backend: Option<BackendKind>,
        #[arg(long)]
        database: Option<PathBuf>,
        #[arg(long)]
        binary: Option<PathBuf>,
        #[arg(long)]
        dry_run: bool,
    },
    Update {
        #[arg(long)]
        version: Option<String>,
        #[arg(long)]
        install_dir: Option<PathBuf>,
        #[arg(long)]
        release_base_url: Option<String>,
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        force: bool,
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

impl TryFrom<&str> for BackendKind {
    type Error = SingletonError;

    fn try_from(value: &str) -> Result<Self> {
        match value {
            "fake" => Ok(Self::Fake),
            "copilot" => Ok(Self::Copilot),
            other => Err(SingletonError::InvalidInput(format!(
                "unsupported backend '{other}'"
            ))),
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
    lock: PathBuf,
}

struct DaemonStartupLock {
    file: File,
}

#[derive(Debug, Clone, Default)]
struct ConfigArgs {
    config: Option<PathBuf>,
    profile: Option<String>,
    no_project_config: bool,
}

impl DaemonStartupLock {
    fn acquire(paths: &StatePaths) -> Result<Self> {
        if let Some(parent) = paths.lock.parent() {
            fs::create_dir_all(parent).map_err(|error| {
                SingletonError::Store(format!(
                    "create daemon lock directory {}: {error}",
                    parent.display()
                ))
            })?;
        }
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&paths.lock)
            .map_err(|error| {
                SingletonError::Store(format!(
                    "open daemon lock {}: {error}",
                    paths.lock.display()
                ))
            })?;
        file.lock_exclusive().map_err(|error| {
            SingletonError::Store(format!(
                "lock daemon lock {}: {error}",
                paths.lock.display()
            ))
        })?;
        Ok(Self { file })
    }
}

impl Drop for DaemonStartupLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DaemonState {
    Running,
    Stopped,
    StalePid,
    StaleSocket,
    StalePidAndSocket,
    Degraded,
}

impl DaemonState {
    fn label(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Stopped => "stopped",
            Self::StalePid => "stale pid",
            Self::StaleSocket => "stale socket",
            Self::StalePidAndSocket => "stale pid and socket",
            Self::Degraded => "degraded",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum PidFileState {
    Missing,
    Alive(u32),
    Dead(u32),
    Invalid(String),
}

impl PidFileState {
    fn describe(&self) -> String {
        match self {
            Self::Missing => "missing".to_string(),
            Self::Alive(pid) => format!("alive pid={pid}"),
            Self::Dead(pid) => format!("stale pid={pid}"),
            Self::Invalid(value) => format!("invalid pid={value}"),
        }
    }

    fn alive_pid(&self) -> Option<u32> {
        match self {
            Self::Alive(pid) => Some(*pid),
            Self::Missing | Self::Dead(_) | Self::Invalid(_) => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SocketFileState {
    Missing,
    Listening,
    Stale,
}

impl SocketFileState {
    fn describe(self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::Listening => "listening",
            Self::Stale => "stale",
        }
    }

    fn is_listening(self) -> bool {
        matches!(self, Self::Listening)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct DaemonStatus {
    state: DaemonState,
    pid: PidFileState,
    socket: SocketFileState,
}

impl DaemonStatus {
    fn cleanup_recommended(&self) -> bool {
        matches!(
            self.state,
            DaemonState::StalePid | DaemonState::StaleSocket | DaemonState::StalePidAndSocket
        ) || (self.state == DaemonState::Degraded && !self.socket.is_listening())
    }
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
    let config_args = ConfigArgs {
        config: cli.config,
        profile: cli.profile,
        no_project_config: cli.no_project_config,
    };
    match cli.command {
        Command::Serve {
            database,
            backend,
            once,
            stdio,
            daemon,
            direct,
        } => serve(&config_args, database, backend, once, stdio, daemon, direct).await,
        Command::Start { database, backend } => start(&config_args, database, backend).await,
        Command::Status { database } => status(&config_args, database).await,
        Command::Stop { database } => stop(&config_args, database).await,
        Command::McpConfig { backend, database } => mcp_config(&config_args, backend, database),
        Command::InstallMcp {
            client,
            name,
            backend,
            database,
            binary,
            dry_run,
        } => install_mcp(
            &config_args,
            client,
            &name,
            backend,
            database,
            binary,
            dry_run,
        ),
        Command::Update {
            version,
            install_dir,
            release_base_url,
            dry_run,
            force,
        } => {
            run_update_command(
                &config_args,
                version,
                install_dir,
                release_base_url,
                dry_run,
                force,
            )
            .await
        }
    }
}

async fn serve(
    config_args: &ConfigArgs,
    database: Option<PathBuf>,
    backend: Option<BackendKind>,
    once: bool,
    stdio: bool,
    daemon: bool,
    direct: bool,
) -> Result<()> {
    let effective = resolve_effective_config(config_args, database, backend)?;
    let backend = BackendKind::try_from(effective.backend.as_str())?;
    let paths = resolve_state_paths(effective.database.clone())?;
    if stdio && !direct && !daemon && !once {
        return proxy_stdio_to_daemon(paths, backend, config_args).await;
    }
    if daemon {
        return run_backend(effective, backend, ServeMode::Daemon(paths)).await;
    }
    let mode = if stdio {
        ServeMode::DirectStdio
    } else if once {
        ServeMode::Once
    } else {
        ServeMode::Foreground
    };
    run_backend(effective, backend, mode).await
}

async fn start(
    config_args: &ConfigArgs,
    database: Option<PathBuf>,
    backend: Option<BackendKind>,
) -> Result<()> {
    let effective = resolve_effective_config(config_args, database, backend)?;
    let backend = BackendKind::try_from(effective.backend.as_str())?;
    let paths = resolve_state_paths(effective.database)?;
    ensure_daemon_running(&paths, backend, config_args).await?;
    let daemon = inspect_daemon(&paths).await?;
    let pid = match daemon.pid {
        PidFileState::Alive(pid) => pid.to_string(),
        PidFileState::Missing | PidFileState::Dead(_) | PidFileState::Invalid(_) => {
            "unknown".to_string()
        }
    };
    println!(
        "singletond running: pid={}, socket={}",
        pid,
        paths.socket.display(),
    );
    Ok(())
}

async fn status(config_args: &ConfigArgs, database: Option<PathBuf>) -> Result<()> {
    let effective = resolve_effective_config(config_args, database, None)?;
    let paths = resolve_state_paths(effective.database.clone())?;
    let daemon = inspect_daemon(&paths).await?;
    println!("daemon: {}", daemon.state.label());
    println!("database: {}", paths.database.display());
    println!("pid: {} ({})", paths.pid.display(), daemon.pid.describe());
    println!(
        "socket: {} ({})",
        paths.socket.display(),
        daemon.socket.describe()
    );
    println!("startup_lock: {}", paths.lock.display());
    if daemon.cleanup_recommended() {
        println!("cleanup: {}", cleanup_command(&paths.database));
    } else if daemon.state == DaemonState::Degraded {
        println!("warning: daemon socket is accepting connections but pid state is degraded");
    }
    let store = Store::open(paths.database)?;
    let sessions = store.list_sessions()?;
    println!("sessions: {}", sessions.len());
    for session in sessions {
        println!(
            "{}\t{:?}\t{}",
            session.session_id, session.status, session.title
        );
    }
    let ssh_hosts = effective
        .hosts
        .iter()
        .filter_map(|(host_id, host)| match host {
            EffectiveHostConfig::Ssh { target, .. } => Some((host_id, target)),
            EffectiveHostConfig::Local { .. } => None,
        })
        .collect::<Vec<_>>();
    if !ssh_hosts.is_empty() {
        let health = store
            .list_remote_host_health()?
            .into_iter()
            .map(|health| (health.host_id.clone(), health))
            .collect::<std::collections::BTreeMap<_, _>>();
        println!("ssh_hosts: {}", ssh_hosts.len());
        for (host_id, target) in ssh_hosts {
            let state = health
                .get(host_id)
                .map(|health| format!("{:?}", health.state))
                .unwrap_or_else(|| "NotChecked".to_string());
            let last_checked = health
                .get(host_id)
                .and_then(|health| health.last_checked_at.as_deref())
                .unwrap_or("never");
            println!("{host_id}\tstate={state}\ttarget={target}\tlast_checked={last_checked}");
        }
    }
    Ok(())
}

async fn stop(config_args: &ConfigArgs, database: Option<PathBuf>) -> Result<()> {
    let effective = resolve_effective_config(config_args, database, None)?;
    let paths = resolve_state_paths(effective.database)?;
    let daemon = inspect_daemon(&paths).await?;
    if let Some(pid) = daemon.pid.alive_pid() {
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
    } else if daemon.socket.is_listening() {
        return Err(SingletonError::InvalidState(format!(
            "daemon socket {} is accepting connections, but pid file {} is not usable; refusing to remove a live socket without a live pid",
            paths.socket.display(),
            paths.pid.display()
        )));
    }
    cleanup_daemon_files(&paths)?;
    println!("singletond stopped");
    Ok(())
}

fn mcp_config(
    config_args: &ConfigArgs,
    backend: Option<BackendKind>,
    database: Option<PathBuf>,
) -> Result<()> {
    let explicit_database = database.is_some();
    let effective = resolve_effective_config(config_args, database, backend)?;
    let backend = BackendKind::try_from(effective.backend.as_str())?;
    let database = database_arg_for_server(&effective, explicit_database)?;
    let executable = resolve_singleton_binary(None)?;
    let args = singleton_server_args(config_args, backend, database);
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
    config_args: &ConfigArgs,
    client: McpClientKind,
    name: &str,
    backend: Option<BackendKind>,
    database: Option<PathBuf>,
    binary: Option<PathBuf>,
    dry_run: bool,
) -> Result<()> {
    let explicit_database = database.is_some();
    let effective = resolve_effective_config(config_args, database, backend)?;
    let backend = BackendKind::try_from(effective.backend.as_str())?;
    let database = database_arg_for_server(&effective, explicit_database)?;
    let command = install_mcp_command(client, name, config_args, backend, database, binary)?;
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

async fn run_update_command(
    config_args: &ConfigArgs,
    version: Option<String>,
    install_dir: Option<PathBuf>,
    release_base_url: Option<String>,
    dry_run: bool,
    force: bool,
) -> Result<()> {
    let outcome = update::run(update::UpdateOptions {
        version,
        install_dir,
        release_base_url,
        dry_run,
        force,
    })?;
    match outcome {
        update::UpdateOutcome::DryRun(plan) => {
            println!("target: {}", plan.target.display());
            println!("platform: {}", plan.target_triple);
            println!("archive: {}", plan.archive_url);
            println!("checksum: {}", plan.checksum_url);
        }
        update::UpdateOutcome::UpToDate { target, version } => {
            println!(
                "singleton already up to date: version {version} at {}",
                target.display()
            );
        }
        update::UpdateOutcome::Updated {
            target,
            previous_version,
            version,
        } => {
            let previous = previous_version.unwrap_or_else(|| "not installed".to_string());
            println!(
                "updated singleton: {previous} -> {version} at {}",
                target.display()
            );
            warn_if_daemon_running(config_args).await;
        }
    }
    Ok(())
}

async fn warn_if_daemon_running(config_args: &ConfigArgs) {
    let effective = match resolve_effective_config(config_args, None, None) {
        Ok(effective) => effective,
        Err(error) => {
            eprintln!("warning: could not inspect singleton daemon after update: {error}");
            return;
        }
    };
    let paths = match resolve_state_paths(effective.database) {
        Ok(paths) => paths,
        Err(error) => {
            eprintln!("warning: could not inspect singleton daemon after update: {error}");
            return;
        }
    };
    match inspect_daemon(&paths).await {
        Ok(daemon) if daemon.state == DaemonState::Running => {
            println!(
                "note: singletond is running; stop it with '{}' so the next start uses the updated binary",
                cleanup_command(&paths.database)
            );
        }
        Ok(_) => {}
        Err(error) => {
            eprintln!("warning: could not inspect singleton daemon after update: {error}");
        }
    }
}

fn resolve_effective_config(
    args: &ConfigArgs,
    database: Option<PathBuf>,
    backend: Option<BackendKind>,
) -> Result<EffectiveConfig> {
    let env = ConfigEnvironment::from_process();
    let cwd = std::env::current_dir().map_err(|error| {
        SingletonError::InvalidState(format!("read current directory: {error}"))
    })?;
    let mut options = ConfigLoadOptions::new(cwd, env);
    options.config_path = args.config.clone();
    options.profile = args.profile.clone();
    options.no_project_config = args.no_project_config;
    let mut overrides = ConfigFieldOverrides::default();
    if let Some(database) = database {
        overrides.database = Some(database);
    }
    if let Some(backend) = backend {
        overrides.backend = Some(backend.as_str().to_string());
    }
    options.cli_overrides = overrides;
    load_effective_config(options)
}

fn database_arg_for_server(
    effective: &EffectiveConfig,
    explicit_database: bool,
) -> Result<Option<PathBuf>> {
    let env = ConfigEnvironment::from_process();
    database_arg_for_server_with_env(effective, explicit_database, &env)
}

fn database_arg_for_server_with_env(
    effective: &EffectiveConfig,
    explicit_database: bool,
    env: &ConfigEnvironment,
) -> Result<Option<PathBuf>> {
    let default_database = default_database_path(env);
    if explicit_database
        || default_database
            .as_ref()
            .map_or(true, |default| effective.database != *default)
    {
        Ok(Some(effective.database.clone()))
    } else {
        Ok(None)
    }
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
    config_args: &ConfigArgs,
    backend: BackendKind,
    database: Option<PathBuf>,
    binary: Option<PathBuf>,
) -> Result<CommandSpec> {
    let binary = resolve_singleton_binary(binary)?;
    let mut server_args = vec![binary];
    server_args.extend(singleton_server_args(config_args, backend, database));
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

fn singleton_server_args(
    config_args: &ConfigArgs,
    backend: BackendKind,
    database: Option<PathBuf>,
) -> Vec<String> {
    let mut args = vec![
        "serve".to_string(),
        "--stdio".to_string(),
        "--backend".to_string(),
        backend.as_str().to_string(),
    ];
    append_config_flags(&mut args, config_args);
    if let Some(database) = database {
        args.push("--database".to_string());
        args.push(database.to_string_lossy().to_string());
    }
    args
}

fn append_config_flags(args: &mut Vec<String>, config_args: &ConfigArgs) {
    if let Some(config) = &config_args.config {
        args.push("--config".to_string());
        args.push(config.to_string_lossy().to_string());
    }
    if let Some(profile) = &config_args.profile {
        args.push("--profile".to_string());
        args.push(profile.clone());
    }
    if config_args.no_project_config {
        args.push("--no-project-config".to_string());
    }
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

async fn run_backend(
    effective: EffectiveConfig,
    backend: BackendKind,
    mode: ServeMode,
) -> Result<()> {
    let store = Store::open(&effective.database)?;
    let default_profile = effective.profile.clone();
    let capability_defaults = effective.capability_defaults();
    let remote_registry = remote_registry_from_effective(&effective, store.clone());
    match backend {
        BackendKind::Fake => {
            let broker = Broker::new_with_reconnect(store, FakeBackend::new(), LocalHostConnector)
                .await?
                .with_capability_defaults(default_profile, capability_defaults);
            let broker = attach_remote_registry(broker, remote_registry);
            run_broker(broker, mode).await
        }
        BackendKind::Copilot => {
            let cwd = std::env::current_dir().map_err(|error| {
                SingletonError::InvalidState(format!("read current directory: {error}"))
            })?;
            let backend = CopilotBackend::new(cwd).with_request_store(store.clone());
            let broker = Broker::new_with_reconnect(store, backend, LocalHostConnector)
                .await?
                .with_capability_defaults(default_profile, capability_defaults);
            let broker = attach_remote_registry(broker, remote_registry);
            run_broker(broker, mode).await
        }
    }
}

fn remote_registry_from_effective(
    effective: &EffectiveConfig,
    store: Store,
) -> Option<Arc<dyn RemoteBrokerRegistry>> {
    SshRemoteBrokerRegistry::from_effective_config_with_store(effective, Some(store))
        .map(|registry| Arc::new(registry) as Arc<dyn RemoteBrokerRegistry>)
}

fn attach_remote_registry<B>(
    broker: Broker<B, LocalHostConnector>,
    remote_registry: Option<Arc<dyn RemoteBrokerRegistry>>,
) -> Broker<B, LocalHostConnector>
where
    B: AgentBackend + 'static,
{
    if let Some(remote_registry) = remote_registry {
        spawn_remote_warmup(remote_registry.clone());
        broker.with_remote_registry(remote_registry)
    } else {
        broker
    }
}

fn spawn_remote_warmup(remote_registry: Arc<dyn RemoteBrokerRegistry>) {
    tokio::spawn(async move {
        let _ = remote_registry.warmup_all().await;
    });
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
    let listener = if daemon_startup_lock_is_held(&paths) {
        bind_daemon_listener(&paths).await?
    } else {
        let _lock = DaemonStartupLock::acquire(&paths)?;
        bind_daemon_listener(&paths).await?
    };
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

async fn bind_daemon_listener(paths: &StatePaths) -> Result<UnixListener> {
    if daemon_socket_ready(paths).await {
        return Err(SingletonError::InvalidState(format!(
            "daemon already listening on {}",
            paths.socket.display()
        )));
    }
    remove_stale_socket(paths)?;
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
    Ok(listener)
}

async fn proxy_stdio_to_daemon(
    paths: StatePaths,
    backend: BackendKind,
    config_args: &ConfigArgs,
) -> Result<()> {
    ensure_daemon_running(&paths, backend, config_args).await?;
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

async fn ensure_daemon_running(
    paths: &StatePaths,
    backend: BackendKind,
    config_args: &ConfigArgs,
) -> Result<()> {
    if daemon_socket_ready(paths).await {
        return Ok(());
    }
    let _lock = DaemonStartupLock::acquire(paths)?;
    if daemon_socket_ready(paths).await {
        return Ok(());
    }
    let executable = std::env::current_exe().map_err(|error| {
        SingletonError::InvalidState(format!("locate singleton binary: {error}"))
    })?;
    let database_arg = paths.database.to_string_lossy().to_string();
    let mut args = Vec::new();
    append_config_flags(&mut args, config_args);
    args.extend([
        "serve".to_string(),
        "--daemon".to_string(),
        "--backend".to_string(),
        backend.as_str().to_string(),
        "--database".to_string(),
        database_arg,
    ]);
    let mut command = ProcessCommand::new(executable);
    command
        .args(args)
        .env(DAEMON_STARTUP_LOCK_HELD_ENV, &paths.lock)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command.process_group(0);
    let mut child = command
        .spawn()
        .map_err(|error| SingletonError::InvalidState(format!("spawn singletond: {error}")))?;
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if inspect_daemon(paths).await?.state == DaemonState::Running {
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

fn daemon_startup_lock_is_held(paths: &StatePaths) -> bool {
    std::env::var_os(DAEMON_STARTUP_LOCK_HELD_ENV)
        .is_some_and(|value| value == paths.lock.as_os_str())
}

async fn inspect_daemon(paths: &StatePaths) -> Result<DaemonStatus> {
    let socket = if daemon_socket_ready(paths).await {
        SocketFileState::Listening
    } else if paths.socket.exists() {
        SocketFileState::Stale
    } else {
        SocketFileState::Missing
    };
    let pid = inspect_pid_file(paths)?;
    let state = match (&pid, socket) {
        (PidFileState::Alive(_), SocketFileState::Listening) => DaemonState::Running,
        (PidFileState::Missing, SocketFileState::Missing) => DaemonState::Stopped,
        (PidFileState::Dead(_) | PidFileState::Invalid(_), SocketFileState::Missing) => {
            DaemonState::StalePid
        }
        (PidFileState::Missing, SocketFileState::Stale) => DaemonState::StaleSocket,
        (PidFileState::Dead(_) | PidFileState::Invalid(_), SocketFileState::Stale) => {
            DaemonState::StalePidAndSocket
        }
        (PidFileState::Alive(_), SocketFileState::Missing | SocketFileState::Stale)
        | (
            PidFileState::Missing | PidFileState::Dead(_) | PidFileState::Invalid(_),
            SocketFileState::Listening,
        ) => DaemonState::Degraded,
    };
    Ok(DaemonStatus { state, pid, socket })
}

fn inspect_pid_file(paths: &StatePaths) -> Result<PidFileState> {
    if !paths.pid.exists() {
        return Ok(PidFileState::Missing);
    }
    let pid_text = fs::read_to_string(&paths.pid)
        .map_err(|error| SingletonError::Store(format!("read {}: {error}", paths.pid.display())))?;
    let trimmed = pid_text.trim();
    if trimmed.is_empty() {
        return Ok(PidFileState::Invalid("<empty>".to_string()));
    }
    let pid = match trimmed.parse::<u32>() {
        Ok(pid) => pid,
        Err(_) => return Ok(PidFileState::Invalid(trimmed.to_string())),
    };
    if process_alive(pid)? {
        Ok(PidFileState::Alive(pid))
    } else {
        Ok(PidFileState::Dead(pid))
    }
}

fn cleanup_command(database: &Path) -> String {
    format!(
        "singleton stop --database {}",
        shell_quote(&database.to_string_lossy())
    )
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

fn resolve_state_paths(database: PathBuf) -> Result<StatePaths> {
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
    let lock = directory.join(format!("{stem}.lock"));
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
        lock,
    })
}

#[cfg(test)]
fn resolve_database(database: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(database) = database {
        return Ok(database);
    }
    default_database_path(&ConfigEnvironment::from_process())
}

#[cfg(test)]
mod tests {
    use tempfile::{NamedTempFile, TempDir};

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
        let paths = resolve_state_paths(file.path().to_path_buf())?;
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
    fn mcp_database_arg_uses_resolved_database_when_home_is_missing() -> Result<()> {
        let temp = TempDir::new()
            .map_err(|error| SingletonError::Store(format!("create temp dir: {error}")))?;
        let database = temp.path().join("configured.db");
        let mut options = ConfigLoadOptions::new(temp.path(), ConfigEnvironment::new(None, None));
        options.cli_overrides = ConfigFieldOverrides::default().with_database(database.clone());
        let effective = load_effective_config(options)?;

        assert_eq!(
            database_arg_for_server_with_env(
                &effective,
                false,
                &ConfigEnvironment::new(None, None)
            )?,
            Some(database)
        );
        Ok(())
    }

    #[test]
    fn install_mcp_builds_copilot_command() -> Result<()> {
        let command = install_mcp_command(
            McpClientKind::Copilot,
            "singleton",
            &ConfigArgs::default(),
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
            &ConfigArgs::default(),
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
            &ConfigArgs::default(),
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
    fn install_mcp_preserves_config_selection_flags() -> Result<()> {
        let config_args = ConfigArgs {
            config: Some(PathBuf::from("/tmp/singleton.toml")),
            profile: Some("work".to_string()),
            no_project_config: true,
        };
        let command = install_mcp_command(
            McpClientKind::Copilot,
            "singleton",
            &config_args,
            BackendKind::Copilot,
            None,
            Some(PathBuf::from("singleton")),
        )?;
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
                "copilot",
                "--config",
                "/tmp/singleton.toml",
                "--profile",
                "work",
                "--no-project-config"
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
