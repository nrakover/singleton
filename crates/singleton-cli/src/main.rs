use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};
use rmcp::ServiceExt;
use singleton_broker::Broker;
use singleton_copilot::CopilotBackend;
use singleton_core::{AgentBackend, Result, SingletonError};
use singleton_host::LocalHostConnector;
use singleton_mcp::SingletonMcpServer;
use singleton_store::Store;
use singleton_test_support::FakeBackend;

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
    },
    Status {
        #[arg(long)]
        database: Option<PathBuf>,
    },
    Stop,
}

#[derive(Clone, Debug, ValueEnum)]
enum BackendKind {
    Fake,
    Copilot,
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
        } => serve(database, backend, once, stdio).await,
        Command::Status { database } => status(database),
        Command::Stop => {
            println!(
                "singletond stop is not active yet; no daemon pid file is managed by this build"
            );
            Ok(())
        }
    }
}

async fn serve(
    database: Option<PathBuf>,
    backend: BackendKind,
    once: bool,
    stdio: bool,
) -> Result<()> {
    let store = Store::open(resolve_database(database)?)?;
    match backend {
        BackendKind::Fake => {
            run_broker(
                Broker::new(store, FakeBackend::new(), LocalHostConnector),
                once,
                stdio,
            )
            .await
        }
        BackendKind::Copilot => {
            let cwd = std::env::current_dir().map_err(|error| {
                SingletonError::InvalidState(format!("read current directory: {error}"))
            })?;
            let backend = CopilotBackend::new(cwd).with_request_store(store.clone());
            run_broker(Broker::new(store, backend, LocalHostConnector), once, stdio).await
        }
    }
}

async fn run_broker<B>(broker: Broker<B, LocalHostConnector>, once: bool, stdio: bool) -> Result<()>
where
    B: AgentBackend + 'static,
{
    if stdio {
        let server = SingletonMcpServer::new(broker);
        let service = server
            .serve(rmcp::transport::io::stdio())
            .await
            .map_err(|error| SingletonError::InvalidState(format!("start MCP stdio: {error}")))?;
        service
            .waiting()
            .await
            .map_err(|error| SingletonError::InvalidState(format!("run MCP stdio: {error}")))?;
        return Ok(());
    }
    let capabilities = broker.get_capabilities();
    println!(
        "singletond ready: protocol={}, tools={}",
        capabilities.protocol_version,
        capabilities.tools.join(",")
    );
    if once {
        return Ok(());
    }
    std::future::pending::<()>().await;
    Ok(())
}

fn status(database: Option<PathBuf>) -> Result<()> {
    let store = Store::open(resolve_database(database)?)?;
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

fn resolve_database(database: Option<PathBuf>) -> Result<PathBuf> {
    if let Some(database) = database {
        return Ok(database);
    }
    let home = std::env::var_os("HOME").ok_or_else(|| {
        SingletonError::InvalidInput("HOME is not set; pass --database explicitly".to_string())
    })?;
    let dir = PathBuf::from(home).join(".singleton");
    std::fs::create_dir_all(&dir).map_err(|error| {
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
}
