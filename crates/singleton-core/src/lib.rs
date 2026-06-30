use std::sync::Arc;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

pub type HostId = String;
pub type WorkspaceId = String;
pub type SessionId = String;
pub type ChatId = String;
pub type TurnId = String;
pub type RequestId = String;
pub type EventId = String;
pub type BackendId = String;
pub type BackendSessionId = String;
pub type BackendTurnId = String;
pub type OperationId = String;

pub const LOCAL_HOST_ID: &str = "host_local";
pub const COPILOT_BACKEND_ID: &str = "copilot";
pub const FAKE_BACKEND_ID: &str = "fake";

pub const DEFAULT_MCP_TOOLS: &[&str] = &[
    "get_capabilities",
    "get_inbox",
    "ack_inbox",
    "ensure_workspace",
    "create_session",
    "send_message",
    "read_events",
    "get_latest_output",
    "list_sessions",
    "get_session",
    "resolve_request",
    "cancel_turn",
    "close_resource",
];

#[derive(Debug, thiserror::Error)]
pub enum SingletonError {
    #[error("{resource} not found: {id}")]
    NotFound { resource: &'static str, id: String },
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("invalid state transition: {0}")]
    InvalidState(String),
    #[error("backend error from {backend}: {message}")]
    Backend { backend: String, message: String },
    #[error("host error from {host}: {message}")]
    Host { host: String, message: String },
    #[error("store error: {0}")]
    Store(String),
}

pub type Result<T> = std::result::Result<T, SingletonError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ResourceKind {
    Root,
    Host,
    Workspace,
    Session,
    Chat,
    Turn,
    Request,
    Changeset,
    Terminal,
    Artifact,
}

impl ResourceKind {
    pub fn slug(self) -> &'static str {
        match self {
            Self::Root => "root",
            Self::Host => "host",
            Self::Workspace => "workspace",
            Self::Session => "session",
            Self::Chat => "chat",
            Self::Turn => "turn",
            Self::Request => "request",
            Self::Changeset => "changeset",
            Self::Terminal => "terminal",
            Self::Artifact => "artifact",
        }
    }

    pub fn resource_name(self) -> &'static str {
        match self {
            Self::Root => "root",
            Self::Host => "host",
            Self::Workspace => "workspace",
            Self::Session => "session",
            Self::Chat => "chat",
            Self::Turn => "turn",
            Self::Request => "request",
            Self::Changeset => "changeset",
            Self::Terminal => "terminal",
            Self::Artifact => "artifact",
        }
    }
}

pub fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", Uuid::now_v7().simple())
}

pub fn resource_uri(kind: ResourceKind, id: &str) -> String {
    if kind == ResourceKind::Root {
        "singleton-root://".to_string()
    } else {
        format!("singleton-{}:/{id}", kind.slug())
    }
}

pub fn now_rfc3339() -> String {
    OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_string())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HostKind {
    Local,
    Ssh,
    Cloud,
    Ahp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct HostCapabilities {
    pub workspace_providers: Vec<String>,
    pub agent_backends: Vec<String>,
    pub supports_reconnect: bool,
    pub supports_ordered_events: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Host {
    pub host_id: HostId,
    pub resource_uri: String,
    pub kind: HostKind,
    pub status: ResourceStatus,
    pub capabilities: HostCapabilities,
}

impl Host {
    pub fn local() -> Self {
        Self {
            host_id: LOCAL_HOST_ID.to_string(),
            resource_uri: resource_uri(ResourceKind::Host, LOCAL_HOST_ID),
            kind: HostKind::Local,
            status: ResourceStatus::Ready,
            capabilities: HostCapabilities {
                workspace_providers: vec![
                    "local_path".to_string(),
                    "git_worktree".to_string(),
                    "backend_default".to_string(),
                ],
                agent_backends: vec![FAKE_BACKEND_ID.to_string(), COPILOT_BACKEND_ID.to_string()],
                supports_reconnect: true,
                supports_ordered_events: true,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ResourceStatus {
    Pending,
    Ready,
    Running,
    Idle,
    NeedsInput,
    Completed,
    Failed,
    Cancelled,
    Archived,
    Disposed,
    Deleted,
    Degraded,
    NotChecked,
    Checking,
    Unavailable,
    Incompatible,
    Backoff,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum HostConnectionState {
    Configured,
    NotChecked,
    Unsupported,
    Connecting,
    Handshaking,
    Available,
    Idle,
    Unavailable,
    Incompatible,
    Degraded,
    Backoff,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RemoteBrokerIdentity {
    pub broker_id: String,
    pub protocol_version: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RemoteHostHealth {
    pub host_id: HostId,
    pub state: HostConnectionState,
    pub remote_identity: Option<RemoteBrokerIdentity>,
    pub capabilities: Option<Capabilities>,
    pub last_checked_at: Option<String>,
    pub last_success_at: Option<String>,
    pub last_error: Option<String>,
    pub updated_at: String,
}

impl RemoteHostHealth {
    pub fn not_checked(host_id: impl Into<String>) -> Self {
        Self {
            host_id: host_id.into(),
            state: HostConnectionState::NotChecked,
            remote_identity: None,
            capabilities: None,
            last_checked_at: None,
            last_success_at: None,
            last_error: None,
            updated_at: now_rfc3339(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ForwardedOperationStatus {
    Pending,
    Applied,
    Failed,
    Uncertain,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ForwardedOperation {
    pub operation_id: OperationId,
    pub host_id: HostId,
    pub operation_kind: String,
    pub status: ForwardedOperationStatus,
    pub local_resource_uri: Option<String>,
    pub request: Value,
    pub result: Option<Value>,
    pub error: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl ForwardedOperation {
    pub fn pending(
        operation_id: impl Into<String>,
        host_id: impl Into<String>,
        operation_kind: impl Into<String>,
        request: Value,
    ) -> Self {
        let now = now_rfc3339();
        Self {
            operation_id: operation_id.into(),
            host_id: host_id.into(),
            operation_kind: operation_kind.into(),
            status: ForwardedOperationStatus::Pending,
            local_resource_uri: None,
            request,
            result: None,
            error: None,
            created_at: now.clone(),
            updated_at: now,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RemoteResourceLink {
    pub local_resource_uri: String,
    pub local_resource_kind: ResourceKind,
    pub local_id: String,
    pub host_id: HostId,
    pub remote_resource_uri: String,
    pub remote_id: String,
    pub remote_cursor: i64,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ReadFreshness {
    Fresh,
    Cached,
    Refreshing,
    StaleUnavailable,
    StaleReconcileFailed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReadSyncStatus {
    pub freshness: ReadFreshness,
    pub host_id: Option<HostId>,
    pub checked_at: Option<String>,
    pub message: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CleanupPolicy {
    #[default]
    Keep,
    DeleteOnArchive,
    DeleteOnSuccess,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct RepoMetadata {
    pub root: Option<String>,
    pub remote: Option<String>,
    pub base_ref: Option<String>,
    pub branch: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Workspace {
    pub workspace_id: WorkspaceId,
    pub resource_uri: String,
    pub host_id: HostId,
    pub status: ResourceStatus,
    pub path: Option<String>,
    pub repo: Option<RepoMetadata>,
    pub cleanup_policy: CleanupPolicy,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkspaceSpec {
    ExistingWorkspace {
        workspace_id: WorkspaceId,
    },
    LocalPath {
        path: String,
        host_id: Option<HostId>,
        cleanup_policy: Option<CleanupPolicy>,
    },
    GitWorktree {
        repo: String,
        base_ref: Option<String>,
        branch: Option<String>,
        create_branch: Option<bool>,
        worktree_path_hint: Option<String>,
        host_id: Option<HostId>,
        cleanup_policy: Option<CleanupPolicy>,
    },
    BackendDefault {
        host_id: Option<HostId>,
        cleanup_policy: Option<CleanupPolicy>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Session {
    pub session_id: SessionId,
    pub resource_uri: String,
    pub title: String,
    pub description: Option<String>,
    pub backend: BackendId,
    pub backend_session_id: Option<BackendSessionId>,
    pub workspace_id: Option<WorkspaceId>,
    pub status: ResourceStatus,
    pub latest_event_cursor: i64,
    pub labels: Vec<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Turn {
    pub turn_id: TurnId,
    pub resource_uri: String,
    pub session_id: SessionId,
    pub backend_turn_id: Option<BackendTurnId>,
    pub message: String,
    pub status: ResourceStatus,
    pub unread: bool,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RequestKind {
    Permission,
    Input,
    Elicitation,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RequestStatus {
    Pending,
    Resolved,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RequestDecision {
    Approve,
    Deny,
    Respond,
    Cancel,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PendingRequest {
    pub request_id: RequestId,
    pub resource_uri: String,
    pub session_id: SessionId,
    pub turn_id: Option<TurnId>,
    pub kind: RequestKind,
    pub status: RequestStatus,
    pub summary: String,
    pub payload: Value,
    pub resolution: Option<Value>,
    pub reason: Option<String>,
    pub created_at: String,
    pub resolved_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Event {
    pub event_id: EventId,
    pub server_seq: i64,
    pub resource_uri: String,
    pub parent_resource_uri: Option<String>,
    pub event_type: String,
    pub origin_kind: String,
    pub origin_id: String,
    pub payload: Value,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LatestOutputSource {
    AssistantMessage,
    TurnSummary,
    ErrorMessage,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LatestOutputEventRef {
    pub event_id: EventId,
    pub server_seq: i64,
    pub event_type: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct LatestOutput {
    pub session_id: SessionId,
    pub turn_id: Option<TurnId>,
    pub turn_resource_uri: Option<String>,
    pub status: Option<ResourceStatus>,
    pub event_cursor: i64,
    pub source_event: Option<LatestOutputEventRef>,
    pub result_text: Option<String>,
    pub result_source: LatestOutputSource,
    pub needs_event_inspection: bool,
    pub inspection_hint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync_status: Option<ReadSyncStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BackendCapabilities {
    pub backend_id: BackendId,
    pub display_name: String,
    pub supports_resume: bool,
    pub supports_turn_reattach: bool,
    pub supports_cancel: bool,
    pub supports_permissions: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BackendSessionConfig {
    pub description: String,
    pub workspace: Option<Workspace>,
    pub model: Option<String>,
    pub mode: Option<String>,
    pub labels: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BackendSession {
    pub backend_id: BackendId,
    pub backend_session_id: BackendSessionId,
    pub status: ResourceStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct BackendMessage {
    pub turn_id: TurnId,
    pub content: String,
    pub mode: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct BackendEvent {
    pub event_type: String,
    pub payload: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct BackendTurn {
    pub backend_turn_id: BackendTurnId,
    pub status: ResourceStatus,
    pub events: Vec<BackendEvent>,
}

pub type BackendEventSink = Arc<dyn Fn(BackendEvent) -> Result<()> + Send + Sync>;

#[async_trait]
pub trait AgentBackend: Send + Sync {
    fn capabilities(&self) -> BackendCapabilities;

    async fn create_session(&self, config: BackendSessionConfig) -> Result<BackendSession>;

    async fn resume_session(&self, id: BackendSessionId) -> Result<BackendSession>;

    async fn send_message(
        &self,
        session: &BackendSession,
        message: BackendMessage,
        event_sink: BackendEventSink,
    ) -> Result<BackendTurn>;

    async fn cancel_turn(&self, session: &BackendSession, turn_id: BackendTurnId) -> Result<()>;

    async fn reattach_turn(
        &self,
        _session: &BackendSession,
        _turn: &Turn,
        _event_sink: BackendEventSink,
    ) -> Result<Option<BackendTurn>> {
        Ok(None)
    }
}

#[async_trait]
pub trait HostConnector: Send + Sync {
    fn host(&self) -> Host;

    async fn ensure_workspace(&self, spec: WorkspaceSpec) -> Result<Workspace>;

    async fn close_workspace(
        &self,
        workspace: &Workspace,
        disposition: CloseDisposition,
        force: bool,
    ) -> Result<CleanupSummary>;
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CloseDisposition {
    #[default]
    Archive,
    Dispose,
    Delete,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
pub struct CleanupSummary {
    pub deleted_paths: Vec<String>,
    pub skipped: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Capabilities {
    pub protocol_version: String,
    pub default_profile: String,
    pub defaults: CapabilityDefaults,
    pub tools: Vec<String>,
    pub hosts: Vec<Host>,
    pub backends: Vec<BackendCapabilities>,
    pub limits: CapabilityLimits,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CapabilityDefaults {
    pub backend: BackendId,
    pub model: Option<String>,
    pub mode: String,
    pub permissions: CapabilityPermissionDefaults,
    pub default_host: HostId,
    pub repo_workspace_provider: String,
    pub cleanup_policy: CleanupPolicy,
}

impl Default for CapabilityDefaults {
    fn default() -> Self {
        Self {
            backend: COPILOT_BACKEND_ID.to_string(),
            model: None,
            mode: "interactive".to_string(),
            permissions: CapabilityPermissionDefaults {
                default: "ask".to_string(),
            },
            default_host: LOCAL_HOST_ID.to_string(),
            repo_workspace_provider: "git_worktree".to_string(),
            cleanup_policy: CleanupPolicy::Keep,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CapabilityPermissionDefaults {
    pub default: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CapabilityLimits {
    pub max_event_limit: usize,
    pub max_wait_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Inbox {
    pub counts: InboxCounts,
    pub items: Vec<InboxItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
pub struct InboxCounts {
    pub permission_request: usize,
    pub input_request: usize,
    pub failed_turn: usize,
    pub completed_turn: usize,
    pub stale_session: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InboxItem {
    PermissionRequest {
        request_id: RequestId,
        session_id: SessionId,
        turn_id: Option<TurnId>,
        summary: String,
        created_at: String,
    },
    InputRequest {
        request_id: RequestId,
        session_id: SessionId,
        prompt: String,
        choices: Option<Vec<String>>,
        created_at: String,
    },
    FailedTurn {
        session_id: SessionId,
        turn_id: TurnId,
        summary: String,
        retryable: bool,
    },
    CompletedTurn {
        session_id: SessionId,
        turn_id: TurnId,
        summary: String,
        unread: bool,
    },
    StaleSession {
        session_id: SessionId,
        reason: String,
    },
}

impl Inbox {
    pub fn empty() -> Self {
        Self {
            counts: InboxCounts::default(),
            items: Vec::new(),
        }
    }

    pub fn push(&mut self, item: InboxItem) {
        match &item {
            InboxItem::PermissionRequest { .. } => self.counts.permission_request += 1,
            InboxItem::InputRequest { .. } => self.counts.input_request += 1,
            InboxItem::FailedTurn { .. } => self.counts.failed_turn += 1,
            InboxItem::CompletedTurn { .. } => self.counts.completed_turn += 1,
            InboxItem::StaleSession { .. } => self.counts.stale_session += 1,
        }
        self.items.push(item);
    }
}

pub fn backend_payload_summary(payload: &Value) -> String {
    payload
        .get("summary")
        .and_then(Value::as_str)
        .or_else(|| payload.get("content").and_then(Value::as_str))
        .unwrap_or("background event")
        .to_string()
}

pub fn compact_json(value: &Value) -> Value {
    match value {
        Value::String(_) | Value::Number(_) | Value::Bool(_) | Value::Null => value.clone(),
        Value::Array(items) => json!({ "items": items.len() }),
        Value::Object(map) => json!({ "keys": map.keys().cloned().collect::<Vec<_>>() }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_uri_uses_stable_scheme() {
        assert_eq!(
            resource_uri(ResourceKind::Root, "ignored"),
            "singleton-root://"
        );
        assert_eq!(
            resource_uri(ResourceKind::Session, "sess_123"),
            "singleton-session:/sess_123"
        );
    }

    #[test]
    fn inbox_counts_are_derived_from_items() {
        let mut inbox = Inbox::empty();
        inbox.push(InboxItem::CompletedTurn {
            session_id: "sess_1".into(),
            turn_id: "turn_1".into(),
            summary: "done".into(),
            unread: true,
        });

        assert_eq!(inbox.counts.completed_turn, 1);
        assert_eq!(inbox.items.len(), 1);
    }

    #[test]
    fn default_tool_profile_stays_small() {
        assert_eq!(DEFAULT_MCP_TOOLS.len(), 13);
        assert!(DEFAULT_MCP_TOOLS.contains(&"get_inbox"));
        assert!(DEFAULT_MCP_TOOLS.contains(&"ack_inbox"));
        assert!(DEFAULT_MCP_TOOLS.contains(&"get_latest_output"));
        assert!(DEFAULT_MCP_TOOLS.contains(&"close_resource"));
    }
}
