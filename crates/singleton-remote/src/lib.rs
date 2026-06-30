use std::collections::BTreeMap;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use singleton_config::{EffectiveConfig, EffectiveHostConfig};
use singleton_core::{
    Capabilities, Host, HostCapabilities, HostConnectionState, HostKind, RemoteBrokerIdentity,
    RemoteBrokerRegistry, RemoteHostHealth, ResourceKind, ResourceStatus, Result, SingletonError,
    resource_uri,
};
use singleton_store::Store;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;

const MCP_PROTOCOL_VERSION: &str = "2024-11-05";
const CONNECT_TIMEOUT: Duration = Duration::from_secs(20);
const CALL_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshHostDescriptor {
    pub host_id: String,
    pub target: String,
    pub connect_command: String,
    pub ssh_args: Vec<String>,
}

#[derive(Clone)]
pub struct SshRemoteBrokerRegistry {
    hosts: Arc<BTreeMap<String, SshHostDescriptor>>,
    health: Arc<Mutex<BTreeMap<String, RemoteHostHealth>>>,
    store: Option<Store>,
}

impl SshRemoteBrokerRegistry {
    pub fn from_effective_config(effective: &EffectiveConfig) -> Option<Self> {
        Self::from_effective_config_with_store(effective, None)
    }

    pub fn from_effective_config_with_store(
        effective: &EffectiveConfig,
        store: Option<Store>,
    ) -> Option<Self> {
        let hosts = effective
            .hosts
            .iter()
            .filter_map(|(host_id, host)| match host {
                EffectiveHostConfig::Ssh {
                    target,
                    connect_command,
                    ssh_args,
                    ..
                } => Some((
                    host_id.clone(),
                    SshHostDescriptor {
                        host_id: host_id.clone(),
                        target: target.clone(),
                        connect_command: connect_command.clone(),
                        ssh_args: ssh_args.clone(),
                    },
                )),
                EffectiveHostConfig::Local { .. } => None,
            })
            .collect::<BTreeMap<_, _>>();
        if hosts.is_empty() {
            return None;
        }
        let health = hosts
            .keys()
            .map(|host_id| (host_id.clone(), RemoteHostHealth::not_checked(host_id)))
            .collect();
        Some(Self {
            hosts: Arc::new(hosts),
            health: Arc::new(Mutex::new(health)),
            store,
        })
    }

    fn descriptor(&self, host_id: &str) -> Result<SshHostDescriptor> {
        self.hosts
            .get(host_id)
            .cloned()
            .ok_or_else(|| SingletonError::Host {
                host: host_id.to_string(),
                message: "unknown SSH host".to_string(),
            })
    }

    fn set_health(
        &self,
        host_id: &str,
        state: HostConnectionState,
        capabilities: Option<Capabilities>,
        last_error: Option<String>,
    ) {
        let mut updated = None;
        if let Ok(mut health) = self.health.lock() {
            let mut current = health
                .remove(host_id)
                .unwrap_or_else(|| RemoteHostHealth::not_checked(host_id));
            let now = singleton_core::now_rfc3339();
            current.state = state.clone();
            current.updated_at = now.clone();
            current.last_checked_at = Some(now.clone());
            if state == HostConnectionState::Available {
                current.last_success_at = Some(now);
            }
            current.last_error = last_error;
            if let Some(capabilities) = capabilities {
                current.remote_identity = Some(RemoteBrokerIdentity {
                    broker_id: format!("ssh:{host_id}"),
                    protocol_version: capabilities.protocol_version.clone(),
                });
                current.capabilities = Some(capabilities);
            }
            updated = Some(current.clone());
            health.insert(host_id.to_string(), current);
        }
        if let (Some(store), Some(updated)) = (&self.store, updated) {
            let _ = store.upsert_remote_host_health(&updated);
        }
    }

    fn mark_connecting(&self, host_id: &str) {
        self.set_health(host_id, HostConnectionState::Connecting, None, None);
    }

    fn host_from_descriptor(&self, descriptor: &SshHostDescriptor) -> Host {
        let health = self.cached_health(&descriptor.host_id);
        let status = health
            .as_ref()
            .map(|health| status_from_connection_state(&health.state))
            .unwrap_or(ResourceStatus::NotChecked);
        let capabilities = health
            .and_then(|health| health.capabilities)
            .map(capabilities_from_remote)
            .unwrap_or_else(|| HostCapabilities {
                workspace_providers: Vec::new(),
                agent_backends: Vec::new(),
                supports_reconnect: true,
                supports_ordered_events: true,
            });
        Host {
            host_id: descriptor.host_id.clone(),
            resource_uri: resource_uri(ResourceKind::Host, &descriptor.host_id),
            kind: HostKind::Ssh,
            status,
            capabilities,
        }
    }
}

#[async_trait]
impl RemoteBrokerRegistry for SshRemoteBrokerRegistry {
    fn hosts(&self) -> Vec<Host> {
        self.hosts
            .values()
            .map(|descriptor| self.host_from_descriptor(descriptor))
            .collect()
    }

    fn cached_health(&self, host_id: &str) -> Option<RemoteHostHealth> {
        self.health
            .lock()
            .ok()
            .and_then(|health| health.get(host_id).cloned())
    }

    async fn call_tool(&self, host_id: &str, tool_name: &str, arguments: Value) -> Result<Value> {
        let descriptor = self.descriptor(host_id)?;
        self.mark_connecting(host_id);
        let result = call_ssh_mcp_tool(&descriptor, tool_name, arguments).await;
        match &result {
            Ok(value) => {
                let capabilities = if tool_name == "get_capabilities" {
                    serde_json::from_value::<Capabilities>(value.clone()).ok()
                } else {
                    None
                };
                self.set_health(host_id, HostConnectionState::Available, capabilities, None);
            }
            Err(error) => {
                self.set_health(
                    host_id,
                    HostConnectionState::Unavailable,
                    None,
                    Some(error.to_string()),
                );
            }
        }
        result
    }

    async fn warmup_all(&self) -> Result<()> {
        for host_id in self.hosts.keys() {
            let _ = self.call_tool(host_id, "get_capabilities", json!({})).await;
        }
        Ok(())
    }
}

async fn call_ssh_mcp_tool(
    descriptor: &SshHostDescriptor,
    tool_name: &str,
    arguments: Value,
) -> Result<Value> {
    let mut command = Command::new("ssh");
    command
        .args(&descriptor.ssh_args)
        .arg(&descriptor.target)
        .arg(&descriptor.connect_command)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().map_err(|error| SingletonError::Host {
        host: descriptor.host_id.clone(),
        message: format!("spawn ssh: {error}"),
    })?;
    let mut stdin = child.stdin.take().ok_or_else(|| SingletonError::Host {
        host: descriptor.host_id.clone(),
        message: "ssh child stdin unavailable".to_string(),
    })?;
    let stdout = child.stdout.take().ok_or_else(|| SingletonError::Host {
        host: descriptor.host_id.clone(),
        message: "ssh child stdout unavailable".to_string(),
    })?;
    let mut stderr = child.stderr.take().ok_or_else(|| SingletonError::Host {
        host: descriptor.host_id.clone(),
        message: "ssh child stderr unavailable".to_string(),
    })?;
    let stderr_task = tokio::spawn(async move {
        let mut text = String::new();
        let _ = stderr.read_to_string(&mut text).await;
        text
    });
    let mut reader = BufReader::new(stdout);

    write_json(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": {},
                "clientInfo": {
                    "name": "singleton-remote",
                    "version": env!("CARGO_PKG_VERSION")
                }
            }
        }),
    )
    .await?;
    let initialized = read_response(&descriptor.host_id, &mut reader, 1, CONNECT_TIMEOUT).await?;
    if initialized.get("result").is_none() {
        return Err(SingletonError::Host {
            host: descriptor.host_id.clone(),
            message: format!("remote MCP initialize failed: {initialized}"),
        });
    }
    write_json(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {}
        }),
    )
    .await?;
    write_json(
        &mut stdin,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": tool_name,
                "arguments": arguments
            }
        }),
    )
    .await?;
    let response = read_response(&descriptor.host_id, &mut reader, 2, CALL_TIMEOUT).await?;
    drop(stdin);
    let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
    let stderr = stderr_task.await.unwrap_or_default();
    if let Some(error) = response.get("error") {
        return Err(SingletonError::Host {
            host: descriptor.host_id.clone(),
            message: format!(
                "remote MCP tool {tool_name} failed: {error}; stderr={}",
                redacted_stderr(&stderr)
            ),
        });
    }
    extract_tool_payload(&descriptor.host_id, tool_name, &response)
}

async fn write_json(stdin: &mut tokio::process::ChildStdin, value: Value) -> Result<()> {
    stdin
        .write_all(value.to_string().as_bytes())
        .await
        .map_err(|error| SingletonError::Host {
            host: "ssh".to_string(),
            message: format!("write MCP request: {error}"),
        })?;
    stdin
        .write_all(b"\n")
        .await
        .map_err(|error| SingletonError::Host {
            host: "ssh".to_string(),
            message: format!("write MCP newline: {error}"),
        })?;
    stdin.flush().await.map_err(|error| SingletonError::Host {
        host: "ssh".to_string(),
        message: format!("flush MCP request: {error}"),
    })
}

async fn read_response(
    host_id: &str,
    reader: &mut BufReader<tokio::process::ChildStdout>,
    expected_id: i64,
    timeout: Duration,
) -> Result<Value> {
    let response = tokio::time::timeout(timeout, async {
        let mut line = String::new();
        loop {
            line.clear();
            let bytes =
                reader
                    .read_line(&mut line)
                    .await
                    .map_err(|error| SingletonError::Host {
                        host: host_id.to_string(),
                        message: format!("read MCP stdout: {error}"),
                    })?;
            if bytes == 0 {
                return Err(SingletonError::Host {
                    host: host_id.to_string(),
                    message: "remote MCP stdout closed".to_string(),
                });
            }
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let value =
                serde_json::from_str::<Value>(trimmed).map_err(|error| SingletonError::Host {
                    host: host_id.to_string(),
                    message: format!("non-JSON stdout before MCP response: {error}"),
                })?;
            if value.get("id").and_then(Value::as_i64) == Some(expected_id) {
                return Ok(value);
            }
        }
    })
    .await;
    response.map_err(|_| SingletonError::Host {
        host: host_id.to_string(),
        message: "timed out waiting for remote MCP response".to_string(),
    })?
}

fn extract_tool_payload(host_id: &str, tool_name: &str, response: &Value) -> Result<Value> {
    let result = response.get("result").ok_or_else(|| SingletonError::Host {
        host: host_id.to_string(),
        message: format!("remote MCP tool {tool_name} returned no result"),
    })?;
    if let Some(payload) = result.get("structuredContent") {
        return Ok(payload.clone());
    }
    if let Some(text) = result
        .get("content")
        .and_then(Value::as_array)
        .and_then(|content| content.first())
        .and_then(|item| item.get("text"))
        .and_then(Value::as_str)
    {
        return serde_json::from_str(text).map_err(|error| SingletonError::Host {
            host: host_id.to_string(),
            message: format!("decode remote MCP text payload for {tool_name}: {error}"),
        });
    }
    Err(SingletonError::Host {
        host: host_id.to_string(),
        message: format!("remote MCP tool {tool_name} returned no structured payload"),
    })
}

fn capabilities_from_remote(capabilities: Capabilities) -> HostCapabilities {
    let workspace_providers = capabilities
        .hosts
        .iter()
        .find(|host| host.kind == HostKind::Local)
        .map(|host| host.capabilities.workspace_providers.clone())
        .unwrap_or_default();
    HostCapabilities {
        workspace_providers,
        agent_backends: capabilities
            .backends
            .iter()
            .map(|backend| backend.backend_id.clone())
            .collect(),
        supports_reconnect: true,
        supports_ordered_events: true,
    }
}

fn status_from_connection_state(state: &HostConnectionState) -> ResourceStatus {
    match state {
        HostConnectionState::Configured | HostConnectionState::NotChecked => {
            ResourceStatus::NotChecked
        }
        HostConnectionState::Unsupported => ResourceStatus::Unsupported,
        HostConnectionState::Connecting | HostConnectionState::Handshaking => {
            ResourceStatus::Checking
        }
        HostConnectionState::Available | HostConnectionState::Idle => ResourceStatus::Ready,
        HostConnectionState::Unavailable => ResourceStatus::Unavailable,
        HostConnectionState::Incompatible => ResourceStatus::Incompatible,
        HostConnectionState::Degraded => ResourceStatus::Degraded,
        HostConnectionState::Backoff => ResourceStatus::Backoff,
    }
}

fn redacted_stderr(stderr: &str) -> String {
    let trimmed = stderr.trim();
    if trimmed.is_empty() {
        return "<empty>".to_string();
    }
    trimmed.lines().take(3).collect::<Vec<_>>().join(" | ")
}

#[cfg(all(test, feature = "live-ssh"))]
mod live_tests {
    use super::*;

    #[tokio::test]
    #[ignore = "requires SINGLETON_LIVE_SSH_TARGET and singleton installed on the remote PATH"]
    async fn live_ssh_get_capabilities_smoke() -> Result<()> {
        let target = std::env::var("SINGLETON_LIVE_SSH_TARGET").map_err(|_| {
            SingletonError::InvalidInput("SINGLETON_LIVE_SSH_TARGET is required".to_string())
        })?;
        let connect_command = std::env::var("SINGLETON_LIVE_SSH_CONNECT_COMMAND")
            .unwrap_or_else(|_| "singleton serve --stdio --backend fake".to_string());
        let descriptor = SshHostDescriptor {
            host_id: "live".to_string(),
            target,
            connect_command,
            ssh_args: vec![
                "-T".to_string(),
                "-o".to_string(),
                "BatchMode=yes".to_string(),
            ],
        };
        let payload = call_ssh_mcp_tool(&descriptor, "get_capabilities", json!({})).await?;
        let capabilities: Capabilities = serde_json::from_value(payload)
            .map_err(|error| SingletonError::InvalidState(error.to_string()))?;
        assert_eq!(capabilities.protocol_version, "0.1");
        Ok(())
    }
}
