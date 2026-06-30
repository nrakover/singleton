use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use singleton_core::{
    AgentBackend, BackendEvent, BackendMessage, BackendSession, BackendSessionConfig, BackendTurn,
    Capabilities, CapabilityDefaults, CapabilityLimits, CleanupSummary, CloseDisposition,
    DEFAULT_MCP_TOOLS, Event, Inbox, InboxItem, LatestOutput, LatestOutputEventRef,
    LatestOutputSource, PendingRequest, RequestDecision, RequestKind, ResourceKind, ResourceStatus,
    Result, Session, SingletonError, Turn, TurnId, Workspace, WorkspaceSpec,
    backend_payload_summary, resource_uri,
};
use singleton_core::{
    HostConnector, LOCAL_HOST_ID, ReadFreshness, ReadSyncStatus, RemoteBrokerRegistry,
    compact_json, new_id, now_rfc3339,
};
use singleton_store::{Store, new_request, new_session, new_turn};

pub struct Broker<B, H>
where
    B: AgentBackend + 'static,
    H: HostConnector + 'static,
{
    store: Store,
    backend: Arc<B>,
    host: Arc<H>,
    remote_registry: Option<Arc<dyn RemoteBrokerRegistry>>,
    default_profile: String,
    defaults: CapabilityDefaults,
}

struct RemoteLinkInsert {
    host_id: String,
    local_resource_uri: String,
    local_resource_kind: ResourceKind,
    local_id: String,
    remote_resource_uri: String,
    remote_id: String,
    remote_cursor: i64,
}

impl<B, H> Clone for Broker<B, H>
where
    B: AgentBackend + 'static,
    H: HostConnector + 'static,
{
    fn clone(&self) -> Self {
        Self {
            store: self.store.clone(),
            backend: self.backend.clone(),
            host: self.host.clone(),
            remote_registry: self.remote_registry.clone(),
            default_profile: self.default_profile.clone(),
            defaults: self.defaults.clone(),
        }
    }
}

impl<B, H> Broker<B, H>
where
    B: AgentBackend,
    H: HostConnector,
{
    pub fn new(store: Store, backend: B, host: H) -> Self {
        let broker = Self {
            store,
            backend: Arc::new(backend),
            host: Arc::new(host),
            remote_registry: None,
            default_profile: "default".to_string(),
            defaults: CapabilityDefaults::default(),
        };
        broker.reconcile_interrupted_turns();
        broker
    }

    pub async fn new_with_reconnect(store: Store, backend: B, host: H) -> Result<Self> {
        let broker = Self {
            store,
            backend: Arc::new(backend),
            host: Arc::new(host),
            remote_registry: None,
            default_profile: "default".to_string(),
            defaults: CapabilityDefaults::default(),
        };
        broker.reconcile_backend_state().await?;
        Ok(broker)
    }

    pub fn with_capability_defaults(
        mut self,
        default_profile: impl Into<String>,
        defaults: CapabilityDefaults,
    ) -> Self {
        self.default_profile = default_profile.into();
        self.defaults = defaults;
        self
    }

    pub fn with_remote_registry(mut self, remote_registry: Arc<dyn RemoteBrokerRegistry>) -> Self {
        self.remote_registry = Some(remote_registry);
        self
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn get_capabilities(&self) -> Capabilities {
        let mut hosts = vec![self.host.host()];
        if let Some(remote_registry) = &self.remote_registry {
            hosts.extend(remote_registry.hosts());
        }
        let mut defaults = self.defaults.clone();
        if !hosts
            .iter()
            .any(|host| host.host_id == defaults.default_host)
            && let Some(host) = hosts.first()
        {
            defaults.default_host = host.host_id.clone();
        }
        Capabilities {
            protocol_version: "0.1".to_string(),
            default_profile: self.default_profile.clone(),
            defaults,
            tools: DEFAULT_MCP_TOOLS.iter().map(ToString::to_string).collect(),
            hosts,
            backends: vec![self.backend.capabilities()],
            limits: CapabilityLimits {
                max_event_limit: 500,
                max_wait_ms: 30_000,
            },
        }
    }

    pub async fn ensure_workspace(&self, spec: WorkspaceSpec) -> Result<Workspace> {
        self.ensure_workspace_request(EnsureWorkspaceRequest {
            spec,
            operation_id: None,
        })
        .await
    }

    pub async fn ensure_workspace_request(
        &self,
        request: EnsureWorkspaceRequest,
    ) -> Result<Workspace> {
        let host_id = self.workspace_spec_target_host(&request.spec)?;
        if host_id != LOCAL_HOST_ID {
            return self.ensure_remote_workspace(host_id, request).await;
        }

        let workspace = match request.spec {
            WorkspaceSpec::ExistingWorkspace { workspace_id } => {
                self.store.get_workspace(&workspace_id)?
            }
            other => {
                let workspace = self.host.ensure_workspace(other).await?;
                self.store.insert_workspace(&workspace)?;
                workspace
            }
        };
        self.store.append_event(
            &workspace.resource_uri,
            None,
            "workspace.ready",
            "singleton",
            "broker",
            json!({ "workspace_id": workspace.workspace_id, "path": workspace.path }),
        )?;
        Ok(workspace)
    }

    pub async fn create_session(
        &self,
        request: CreateSessionRequest,
    ) -> Result<CreateSessionReply> {
        let target_host = self.create_session_target_host(&request)?;
        if target_host != LOCAL_HOST_ID {
            return self.create_remote_session(target_host, request).await;
        }

        let workspace = match request.workspace {
            Some(spec) => Some(self.ensure_workspace(spec).await?),
            None => None,
        };
        let mut session = new_session(
            request.title.unwrap_or_else(|| request.description.clone()),
            request
                .backend
                .unwrap_or_else(|| self.backend.capabilities().backend_id),
            workspace
                .as_ref()
                .map(|workspace| workspace.workspace_id.clone()),
        );
        session.description = Some(request.description.clone());
        session.labels = request.labels.clone();
        self.store.insert_session(&session)?;
        let created = self.store.append_event(
            &session.resource_uri,
            None,
            "session.created",
            "singleton",
            "broker",
            json!({
                "description": request.description,
                "workspace_id": session.workspace_id,
            }),
        )?;

        let backend_session = self
            .backend
            .create_session(BackendSessionConfig {
                description: session.description.clone().unwrap_or_default(),
                workspace,
                model: request.model,
                mode: request.mode,
                labels: request.labels,
            })
            .await?;
        self.store.update_session_backend(
            &session.session_id,
            &backend_session.backend_session_id,
            ResourceStatus::Idle,
        )?;
        let status = self.store.append_event(
            &session.resource_uri,
            None,
            "session.status_changed",
            "backend",
            &backend_session.backend_id,
            json!({
                "status": "idle",
                "backend_session_id": backend_session.backend_session_id,
            }),
        )?;
        self.store.update_session_status(
            &session.session_id,
            ResourceStatus::Idle,
            Some(status.server_seq),
        )?;

        Ok(CreateSessionReply {
            session_id: session.session_id,
            resource_uri: session.resource_uri,
            workspace_id: session.workspace_id,
            status: ResourceStatus::Idle,
            event_cursor: status.server_seq.max(created.server_seq),
        })
    }

    pub async fn send_message(&self, request: SendMessageRequest) -> Result<SendMessageReply> {
        let session = self.store.get_session(&request.session_id)?;
        if self
            .remote_link_for_resource(&session.resource_uri)?
            .is_some()
        {
            return self.send_remote_message(session, request).await;
        }
        let backend_session = backend_session_from(&session)?;
        let mut turn = new_turn(session.session_id.clone(), request.message.clone());
        turn.status = ResourceStatus::Running;
        self.store.insert_turn(&turn)?;
        let queued = self.store.append_event(
            &turn.resource_uri,
            Some(&session.resource_uri),
            "turn.queued",
            "singleton",
            "broker",
            json!({ "message": request.message }),
        )?;
        let started = self.store.append_event(
            &turn.resource_uri,
            Some(&session.resource_uri),
            "turn.started",
            "singleton",
            "broker",
            json!({ "turn_id": turn.turn_id }),
        )?;
        self.store.update_session_status(
            &session.session_id,
            ResourceStatus::Running,
            Some(started.server_seq),
        )?;
        self.spawn_backend_turn(
            &session,
            &turn,
            backend_session,
            request.message,
            request.mode,
            started.server_seq.max(queued.server_seq),
        );
        Ok(SendMessageReply {
            turn_id: turn.turn_id,
            resource_uri: turn.resource_uri,
            status: ResourceStatus::Running,
            event_cursor: started.server_seq.max(queued.server_seq),
        })
    }

    pub async fn read_events(&self, request: ReadEventsRequest) -> Result<ReadEventsReply> {
        let mut sync_status = None;
        if let Some(session_id) = request
            .session_id
            .clone()
            .or_else(|| session_id_from_resource_uri(request.resource_uri.as_deref()))
        {
            sync_status = self
                .sync_remote_session_events(&session_id, request.wait_ms.unwrap_or(0).min(30_000))
                .await;
        }
        let target_uri = match (request.resource_uri, request.session_id) {
            (Some(uri), _) => Some(uri),
            (None, Some(session_id)) => Some(resource_uri(ResourceKind::Session, &session_id)),
            (None, None) => None,
        };
        let wait_ms = request.wait_ms.unwrap_or(0).min(30_000);
        let deadline = Instant::now() + Duration::from_millis(wait_ms);
        loop {
            let events = self.store.read_events(
                target_uri.as_deref(),
                request.cursor.unwrap_or(0),
                request.limit.unwrap_or(100),
                &request.event_types,
            )?;
            if !events.is_empty() || wait_ms == 0 || Instant::now() >= deadline {
                let next_cursor = events
                    .last()
                    .map(|event| event.server_seq)
                    .unwrap_or_else(|| request.cursor.unwrap_or(0));
                return Ok(ReadEventsReply {
                    events,
                    next_cursor,
                    timed_out: wait_ms > 0 && Instant::now() >= deadline,
                    sync_status,
                });
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    pub async fn get_latest_output(&self, request: GetLatestOutputRequest) -> Result<LatestOutput> {
        let sync_status = self
            .sync_remote_session_events(&request.session_id, 0)
            .await;
        let session = self.store.get_session(&request.session_id)?;
        let turn = match request.turn_id {
            Some(turn_id) => {
                let turn = self.store.get_turn(&turn_id)?;
                if turn.session_id != session.session_id {
                    return Err(SingletonError::InvalidInput(format!(
                        "turn {} does not belong to session {}",
                        turn.turn_id, session.session_id
                    )));
                }
                Some(turn)
            }
            None => self
                .store
                .latest_terminal_turn_for_session(&session.session_id)?,
        };
        let Some(turn) = turn else {
            let mut output = no_turn_latest_output(&session);
            output.sync_status = sync_status;
            return Ok(output);
        };
        let events = self
            .store
            .read_recent_events_for_resource(&turn.resource_uri, 500)?;
        let mut output = latest_output_from_events(&session, &turn, &events);
        output.sync_status = sync_status;
        Ok(output)
    }

    pub fn list_sessions(&self) -> Result<Vec<Session>> {
        self.store.list_sessions()
    }

    pub fn get_session(&self, session_id: &str) -> Result<SessionDetail> {
        let session = self.store.get_session(session_id)?;
        let workspace = match &session.workspace_id {
            Some(workspace_id) => Some(self.store.get_workspace(workspace_id)?),
            None => None,
        };
        let active_turn = self.store.active_turn_for_session(session_id)?;
        let pending_requests = self
            .store
            .pending_requests()?
            .into_iter()
            .filter(|request| request.session_id == session_id)
            .collect();
        Ok(SessionDetail {
            session,
            workspace,
            active_turn,
            pending_requests,
        })
    }

    pub fn get_inbox(&self) -> Result<Inbox> {
        let mut inbox = Inbox::empty();
        for request in self.store.pending_requests()? {
            match request.kind {
                RequestKind::Permission => inbox.push(InboxItem::PermissionRequest {
                    request_id: request.request_id,
                    session_id: request.session_id,
                    turn_id: request.turn_id,
                    summary: request.summary,
                    created_at: request.created_at,
                }),
                RequestKind::Input | RequestKind::Elicitation => {
                    let choices = request
                        .payload
                        .get("choices")
                        .and_then(Value::as_array)
                        .map(|choices| {
                            choices
                                .iter()
                                .filter_map(Value::as_str)
                                .map(ToString::to_string)
                                .collect::<Vec<_>>()
                        });
                    inbox.push(InboxItem::InputRequest {
                        request_id: request.request_id,
                        session_id: request.session_id,
                        prompt: request.summary,
                        choices,
                        created_at: request.created_at,
                    });
                }
            }
        }
        for turn in self.store.inbox_turns()? {
            match turn.status {
                ResourceStatus::Completed => inbox.push(InboxItem::CompletedTurn {
                    session_id: turn.session_id,
                    turn_id: turn.turn_id,
                    summary: "turn completed".to_string(),
                    unread: turn.unread,
                }),
                ResourceStatus::Failed => inbox.push(InboxItem::FailedTurn {
                    session_id: turn.session_id,
                    turn_id: turn.turn_id,
                    summary: "turn failed".to_string(),
                    retryable: true,
                }),
                _ => {}
            }
        }
        Ok(inbox)
    }

    pub fn ack_inbox(&self, request: AckInboxRequest) -> Result<AckInboxReply> {
        let acknowledged = self.store.acknowledge_inbox_turns(
            request.session_id.as_deref(),
            request.turn_id.as_deref(),
            request.all,
        )?;
        self.store.append_event(
            "singleton-root://",
            None,
            "inbox.acknowledged",
            "singleton",
            "broker",
            json!({
                "session_id": request.session_id,
                "turn_id": request.turn_id,
                "all": request.all,
                "acknowledged": acknowledged,
            }),
        )?;
        Ok(AckInboxReply { acknowledged })
    }

    pub async fn resolve_request(&self, request: ResolveRequest) -> Result<PendingRequest> {
        let mut operation_host_id = LOCAL_HOST_ID.to_string();
        let mut applied_operation_id = request.operation_id.clone();
        if let Some(link) = self
            .remote_link_for_resource(&resource_uri(ResourceKind::Request, &request.request_id))?
        {
            operation_host_id = link.host_id.clone();
            let operation_id = Self::remote_operation_id(request.operation_id.clone());
            applied_operation_id = Some(operation_id.clone());
            self.record_operation_pending(
                &operation_id,
                &link.host_id,
                "resolve_request",
                &request,
            )?;
            let _: PendingRequest = self
                .call_remote(
                    &link.host_id,
                    "resolve_request",
                    ResolveRequest {
                        request_id: link.remote_id,
                        decision: request.decision.clone(),
                        response: request.response.clone(),
                        reason: request.reason.clone(),
                        operation_id: Some(operation_id.clone()),
                    },
                )
                .await?;
        }
        let resolved = self.store.resolve_request(
            &request.request_id,
            request.decision,
            request.response,
            request.reason,
        )?;
        self.store.append_event(
            &resolved.resource_uri,
            Some(&resource_uri(ResourceKind::Session, &resolved.session_id)),
            "request.resolved",
            "singleton",
            "broker",
            json!({
                "request_id": resolved.request_id,
                "status": resolved.status,
                "reason": resolved.reason,
            }),
        )?;
        if let Some(operation_id) = applied_operation_id {
            self.record_operation_applied(
                &operation_id,
                &operation_host_id,
                "resolve_request",
                Some(resolved.resource_uri.clone()),
                &resolved,
            )?;
        }
        Ok(resolved)
    }

    pub async fn cancel_turn(&self, request: CancelTurnRequest) -> Result<CancelTurnReply> {
        let session = self.store.get_session(&request.session_id)?;
        let turn = match request.turn_id {
            Some(ref turn_id) => self.store.get_turn(turn_id)?,
            None => self
                .store
                .active_turn_for_session(&session.session_id)?
                .ok_or_else(|| SingletonError::InvalidState("session has no active turn".into()))?,
        };
        if let Some(turn_link) = self.remote_link_for_resource(&turn.resource_uri)? {
            let session_link = self
                .remote_link_for_resource(&session.resource_uri)?
                .ok_or_else(|| {
                    SingletonError::InvalidState(format!(
                        "remote turn {} has no remote session link",
                        turn.turn_id
                    ))
                })?;
            let operation_id = Self::remote_operation_id(request.operation_id.clone());
            self.record_operation_pending(
                &operation_id,
                &turn_link.host_id,
                "cancel_turn",
                &request,
            )?;
            let _: CancelTurnReply = self
                .call_remote(
                    &turn_link.host_id,
                    "cancel_turn",
                    CancelTurnRequest {
                        session_id: session_link.remote_id,
                        turn_id: Some(turn_link.remote_id.clone()),
                        operation_id: Some(operation_id.clone()),
                    },
                )
                .await?;
            self.cancel_pending_requests_for_turn(
                &turn,
                "turn cancelled by foreground request".to_string(),
            )?;
            self.store.update_turn_status(
                &turn.turn_id,
                Some(&turn_link.remote_id),
                ResourceStatus::Cancelled,
                true,
            )?;
            let event = self.store.append_event(
                &turn.resource_uri,
                Some(&session.resource_uri),
                "turn.cancelled",
                "singleton",
                "remote-broker",
                json!({ "turn_id": turn.turn_id }),
            )?;
            self.store.update_session_status(
                &session.session_id,
                ResourceStatus::Idle,
                Some(event.server_seq),
            )?;
            let reply = CancelTurnReply {
                cancelled: true,
                turn_id: turn.turn_id,
            };
            self.record_operation_applied(
                &operation_id,
                &turn_link.host_id,
                "cancel_turn",
                Some(turn.resource_uri),
                &reply,
            )?;
            return Ok(reply);
        }
        let backend_session = backend_session_from(&session)?;
        let backend_turn_id = turn
            .backend_turn_id
            .clone()
            .unwrap_or_else(|| turn.turn_id.clone());
        self.cancel_pending_requests_for_turn(
            &turn,
            "turn cancelled by foreground request".to_string(),
        )?;
        self.backend
            .cancel_turn(&backend_session, backend_turn_id.clone())
            .await?;
        self.store.update_turn_status(
            &turn.turn_id,
            Some(&backend_turn_id),
            ResourceStatus::Cancelled,
            true,
        )?;
        let event = self.store.append_event(
            &turn.resource_uri,
            Some(&session.resource_uri),
            "turn.cancelled",
            "singleton",
            "broker",
            json!({ "turn_id": turn.turn_id }),
        )?;
        self.store.update_session_status(
            &session.session_id,
            ResourceStatus::Idle,
            Some(event.server_seq),
        )?;
        Ok(CancelTurnReply {
            cancelled: true,
            turn_id: turn.turn_id,
        })
    }

    pub async fn close_resource(
        &self,
        request: CloseResourceRequest,
    ) -> Result<CloseResourceReply> {
        if let Some(session_id) = request.target.session_id.as_deref()
            && self
                .remote_link_for_resource(&resource_uri(ResourceKind::Session, session_id))?
                .is_some()
        {
            return self.close_remote_resource(request).await;
        }
        if let Some(workspace_id) = request.target.workspace_id.as_deref()
            && self
                .remote_link_for_resource(&resource_uri(ResourceKind::Workspace, workspace_id))?
                .is_some()
        {
            return self.close_remote_resource(request).await;
        }

        let disposition = request.disposition.unwrap_or_default();
        if let Some(session_id) = request.target.session_id {
            let session = self.store.get_session(&session_id)?;
            self.store.close_session(&session_id, disposition.clone())?;
            self.store.append_event(
                &session.resource_uri,
                None,
                "session.archived",
                "singleton",
                "broker",
                json!({ "disposition": disposition }),
            )?;
            return Ok(CloseResourceReply {
                closed: true,
                target_uri: session.resource_uri,
                cleanup_summary: CleanupSummary::default(),
            });
        }

        let workspace_id = match (request.target.workspace_id, request.target.resource_uri) {
            (Some(workspace_id), _) => workspace_id,
            (None, Some(uri)) => uri
                .strip_prefix("singleton-workspace:/")
                .ok_or_else(|| {
                    SingletonError::InvalidInput(format!(
                        "close_resource only supports session_id, workspace_id, or workspace URI: {uri}"
                    ))
                })?
                .to_string(),
            (None, None) => {
                return Err(SingletonError::InvalidInput(
                    "close_resource target is required".to_string(),
                ));
            }
        };
        let workspace = self.store.get_workspace(&workspace_id)?;
        let active_refs = self
            .store
            .active_session_count_for_workspace(&workspace.workspace_id)?;
        if active_refs > 0 && disposition == CloseDisposition::Delete && !request.force {
            return Err(SingletonError::InvalidState(format!(
                "workspace {} has {active_refs} active session reference(s)",
                workspace.workspace_id
            )));
        }
        let cleanup_summary = self
            .host
            .close_workspace(&workspace, disposition.clone(), request.force)
            .await?;
        let status = match disposition {
            CloseDisposition::Archive => ResourceStatus::Archived,
            CloseDisposition::Dispose => ResourceStatus::Disposed,
            CloseDisposition::Delete => ResourceStatus::Deleted,
        };
        self.store
            .update_workspace_status(&workspace.workspace_id, status)?;
        self.store.append_event(
            &workspace.resource_uri,
            None,
            "workspace.closed",
            "singleton",
            "broker",
            json!({ "disposition": disposition, "cleanup": cleanup_summary }),
        )?;
        Ok(CloseResourceReply {
            closed: true,
            target_uri: workspace.resource_uri,
            cleanup_summary,
        })
    }

    fn remote_registry(&self) -> Result<&dyn RemoteBrokerRegistry> {
        self.remote_registry
            .as_deref()
            .ok_or_else(|| SingletonError::Host {
                host: "remote".to_string(),
                message: "no remote broker registry configured".to_string(),
            })
    }

    async fn call_remote<T, R>(&self, host_id: &str, tool_name: &str, arguments: T) -> Result<R>
    where
        T: Serialize,
        R: DeserializeOwned,
    {
        let arguments = serde_json::to_value(arguments).map_err(|error| {
            SingletonError::InvalidState(format!("serialize remote {tool_name} request: {error}"))
        })?;
        let result = self
            .remote_registry()?
            .call_tool(host_id, tool_name, arguments)
            .await?;
        serde_json::from_value(result).map_err(|error| {
            SingletonError::InvalidState(format!(
                "deserialize remote {tool_name} response from host {host_id}: {error}"
            ))
        })
    }

    fn workspace_spec_target_host(&self, spec: &WorkspaceSpec) -> Result<String> {
        match spec {
            WorkspaceSpec::ExistingWorkspace { workspace_id } => {
                Ok(self.store.get_workspace(workspace_id)?.host_id)
            }
            WorkspaceSpec::LocalPath { host_id, .. }
            | WorkspaceSpec::GitWorktree { host_id, .. }
            | WorkspaceSpec::BackendDefault { host_id, .. } => Ok(host_id
                .clone()
                .unwrap_or_else(|| self.defaults.default_host.clone())),
        }
    }

    fn create_session_target_host(&self, request: &CreateSessionRequest) -> Result<String> {
        match &request.workspace {
            Some(spec) => self.workspace_spec_target_host(spec),
            None => Ok(self.defaults.default_host.clone()),
        }
    }

    fn remote_link_for_resource(
        &self,
        resource_uri_value: &str,
    ) -> Result<Option<singleton_core::RemoteResourceLink>> {
        self.store.get_remote_resource_link(resource_uri_value)
    }

    fn remote_spec_for_host(spec: WorkspaceSpec) -> WorkspaceSpec {
        match spec {
            WorkspaceSpec::ExistingWorkspace { workspace_id } => {
                WorkspaceSpec::ExistingWorkspace { workspace_id }
            }
            WorkspaceSpec::LocalPath {
                path,
                cleanup_policy,
                ..
            } => WorkspaceSpec::LocalPath {
                path,
                host_id: None,
                cleanup_policy,
            },
            WorkspaceSpec::GitWorktree {
                repo,
                base_ref,
                branch,
                create_branch,
                worktree_path_hint,
                cleanup_policy,
                ..
            } => WorkspaceSpec::GitWorktree {
                repo,
                base_ref,
                branch,
                create_branch,
                worktree_path_hint,
                host_id: None,
                cleanup_policy,
            },
            WorkspaceSpec::BackendDefault { cleanup_policy, .. } => WorkspaceSpec::BackendDefault {
                host_id: None,
                cleanup_policy,
            },
        }
    }

    fn remote_operation_id(operation_id: Option<String>) -> String {
        operation_id.unwrap_or_else(|| new_id("op"))
    }

    fn applied_operation_result<R: DeserializeOwned>(
        &self,
        operation_id: Option<&str>,
    ) -> Result<Option<R>> {
        let Some(operation_id) = operation_id else {
            return Ok(None);
        };
        let Some(operation) = self.store.get_forwarded_operation(operation_id)? else {
            return Ok(None);
        };
        if operation.status != singleton_core::ForwardedOperationStatus::Applied {
            return Ok(None);
        }
        operation
            .result
            .map(serde_json::from_value)
            .transpose()
            .map_err(|error| {
                SingletonError::InvalidState(format!(
                    "deserialize idempotent operation {operation_id} result: {error}"
                ))
            })
    }

    fn record_operation_pending<T: Serialize>(
        &self,
        operation_id: &str,
        host_id: &str,
        operation_kind: &str,
        request: &T,
    ) -> Result<()> {
        let request = serde_json::to_value(request).map_err(|error| {
            SingletonError::InvalidState(format!(
                "serialize {operation_kind} operation request: {error}"
            ))
        })?;
        self.store
            .upsert_forwarded_operation(&singleton_core::ForwardedOperation::pending(
                operation_id,
                host_id,
                operation_kind,
                request,
            ))
    }

    fn record_operation_applied<R: Serialize>(
        &self,
        operation_id: &str,
        host_id: &str,
        operation_kind: &str,
        local_resource_uri: Option<String>,
        result: &R,
    ) -> Result<()> {
        let mut operation = self
            .store
            .get_forwarded_operation(operation_id)?
            .unwrap_or_else(|| {
                singleton_core::ForwardedOperation::pending(
                    operation_id,
                    host_id,
                    operation_kind,
                    json!({}),
                )
            });
        operation.status = singleton_core::ForwardedOperationStatus::Applied;
        operation.local_resource_uri = local_resource_uri;
        operation.result = Some(serde_json::to_value(result).map_err(|error| {
            SingletonError::InvalidState(format!(
                "serialize {operation_kind} operation result: {error}"
            ))
        })?);
        operation.error = None;
        operation.updated_at = now_rfc3339();
        self.store.upsert_forwarded_operation(&operation)
    }

    fn remote_resource_id(resource_uri_value: &str) -> String {
        resource_uri_value
            .rsplit_once('/')
            .map(|(_, id)| id.to_string())
            .unwrap_or_else(|| resource_uri_value.to_string())
    }

    fn insert_remote_link(&self, link: RemoteLinkInsert) -> Result<()> {
        let now = now_rfc3339();
        self.store
            .upsert_remote_resource_link(&singleton_core::RemoteResourceLink {
                local_resource_uri: link.local_resource_uri,
                local_resource_kind: link.local_resource_kind,
                local_id: link.local_id,
                host_id: link.host_id,
                remote_resource_uri: link.remote_resource_uri,
                remote_id: link.remote_id,
                remote_cursor: link.remote_cursor,
                created_at: now.clone(),
                updated_at: now,
            })
    }

    async fn ensure_remote_workspace(
        &self,
        host_id: String,
        request: EnsureWorkspaceRequest,
    ) -> Result<Workspace> {
        if let WorkspaceSpec::ExistingWorkspace { workspace_id } = &request.spec {
            return self.store.get_workspace(workspace_id);
        }
        if let Some(workspace) = self.applied_operation_result(request.operation_id.as_deref())? {
            return Ok(workspace);
        }
        let operation_id = Self::remote_operation_id(request.operation_id.clone());
        self.record_operation_pending(&operation_id, &host_id, "ensure_workspace", &request)?;
        let remote_spec = Self::remote_spec_for_host(request.spec);
        let remote_workspace: Workspace = self
            .call_remote(
                &host_id,
                "ensure_workspace",
                json!({
                    "spec": remote_spec,
                    "operation_id": operation_id,
                }),
            )
            .await?;

        if let Some(existing) = self
            .store
            .get_remote_resource_link_by_remote(&host_id, &remote_workspace.resource_uri)?
        {
            return self.store.get_workspace(&existing.local_id);
        }

        let workspace_id = new_id("work");
        let workspace = Workspace {
            resource_uri: resource_uri(ResourceKind::Workspace, &workspace_id),
            workspace_id,
            host_id: host_id.clone(),
            status: remote_workspace.status.clone(),
            path: remote_workspace.path.clone(),
            repo: remote_workspace.repo.clone(),
            cleanup_policy: remote_workspace.cleanup_policy.clone(),
            created_at: now_rfc3339(),
        };
        self.store.insert_workspace(&workspace)?;
        self.insert_remote_link(RemoteLinkInsert {
            host_id: host_id.clone(),
            local_resource_uri: workspace.resource_uri.clone(),
            local_resource_kind: ResourceKind::Workspace,
            local_id: workspace.workspace_id.clone(),
            remote_resource_uri: remote_workspace.resource_uri.clone(),
            remote_id: remote_workspace.workspace_id.clone(),
            remote_cursor: 0,
        })?;
        self.store.append_event(
            &workspace.resource_uri,
            None,
            "workspace.ready",
            "singleton",
            "remote-broker",
            json!({
                "host_id": host_id,
                "remote_resource_uri": remote_workspace.resource_uri,
            }),
        )?;
        self.record_operation_applied(
            &operation_id,
            &workspace.host_id,
            "ensure_workspace",
            Some(workspace.resource_uri.clone()),
            &workspace,
        )?;
        Ok(workspace)
    }

    async fn create_remote_session(
        &self,
        host_id: String,
        mut request: CreateSessionRequest,
    ) -> Result<CreateSessionReply> {
        if let Some(reply) = self.applied_operation_result(request.operation_id.as_deref())? {
            return Ok(reply);
        }
        let operation_id = Self::remote_operation_id(request.operation_id.clone());
        self.record_operation_pending(&operation_id, &host_id, "create_session", &request)?;

        let local_workspace = match request.workspace.take() {
            Some(spec) => Some(self.ensure_workspace(spec).await?),
            None => None,
        };
        let remote_workspace = match &local_workspace {
            Some(workspace) => {
                let link = self
                    .remote_link_for_resource(&workspace.resource_uri)?
                    .ok_or_else(|| {
                        SingletonError::InvalidState(format!(
                            "workspace {} on host {host_id} has no remote link",
                            workspace.workspace_id
                        ))
                    })?;
                Some(WorkspaceSpec::ExistingWorkspace {
                    workspace_id: link.remote_id,
                })
            }
            None => None,
        };
        let remote_reply: CreateSessionReply = self
            .call_remote(
                &host_id,
                "create_session",
                CreateSessionRequest {
                    workspace: remote_workspace,
                    operation_id: Some(operation_id.clone()),
                    ..request.clone()
                },
            )
            .await?;
        let remote_detail: SessionDetail = self
            .call_remote(
                &host_id,
                "get_session",
                json!({ "session_id": remote_reply.session_id }),
            )
            .await?;
        let mut session = new_session(
            remote_detail.session.title,
            remote_detail.session.backend,
            local_workspace
                .as_ref()
                .map(|workspace| workspace.workspace_id.clone()),
        );
        session.description = remote_detail.session.description;
        session.labels = remote_detail.session.labels;
        session.status = remote_detail.session.status.clone();
        self.store.insert_session(&session)?;
        let event = self.store.append_event(
            &session.resource_uri,
            None,
            "session.created",
            "singleton",
            "remote-broker",
            json!({
                "description": request.description,
                "host_id": host_id,
                "remote_resource_uri": remote_detail.session.resource_uri,
            }),
        )?;
        self.store.update_session_status(
            &session.session_id,
            remote_detail.session.status.clone(),
            Some(event.server_seq),
        )?;
        self.insert_remote_link(RemoteLinkInsert {
            host_id: host_id.clone(),
            local_resource_uri: session.resource_uri.clone(),
            local_resource_kind: ResourceKind::Session,
            local_id: session.session_id.clone(),
            remote_resource_uri: remote_detail.session.resource_uri.clone(),
            remote_id: remote_detail.session.session_id.clone(),
            remote_cursor: remote_reply.event_cursor,
        })?;
        let reply = CreateSessionReply {
            session_id: session.session_id.clone(),
            resource_uri: session.resource_uri.clone(),
            workspace_id: session.workspace_id.clone(),
            status: session.status.clone(),
            event_cursor: event.server_seq,
        };
        self.record_operation_applied(
            &operation_id,
            &host_id,
            "create_session",
            Some(session.resource_uri),
            &reply,
        )?;
        Ok(reply)
    }

    async fn send_remote_message(
        &self,
        session: Session,
        request: SendMessageRequest,
    ) -> Result<SendMessageReply> {
        if let Some(reply) = self.applied_operation_result(request.operation_id.as_deref())? {
            return Ok(reply);
        }
        let session_link = self
            .remote_link_for_resource(&session.resource_uri)?
            .ok_or_else(|| {
                SingletonError::InvalidState(format!(
                    "session {} has no remote link",
                    session.session_id
                ))
            })?;
        let operation_id = Self::remote_operation_id(request.operation_id.clone());
        self.record_operation_pending(
            &operation_id,
            &session_link.host_id,
            "send_message",
            &request,
        )?;

        let mut turn = new_turn(session.session_id.clone(), request.message.clone());
        turn.status = ResourceStatus::Running;
        self.store.insert_turn(&turn)?;
        let queued = self.store.append_event(
            &turn.resource_uri,
            Some(&session.resource_uri),
            "turn.queued",
            "singleton",
            "remote-broker",
            json!({ "message": request.message }),
        )?;
        let started = self.store.append_event(
            &turn.resource_uri,
            Some(&session.resource_uri),
            "turn.started",
            "singleton",
            "remote-broker",
            json!({ "turn_id": turn.turn_id, "host_id": session_link.host_id }),
        )?;
        self.store.update_session_status(
            &session.session_id,
            ResourceStatus::Running,
            Some(started.server_seq),
        )?;

        let remote_reply: SendMessageReply = match self
            .call_remote(
                &session_link.host_id,
                "send_message",
                SendMessageRequest {
                    session_id: session_link.remote_id.clone(),
                    message: request.message,
                    mode: request.mode,
                    operation_id: Some(operation_id.clone()),
                },
            )
            .await
        {
            Ok(reply) => reply,
            Err(error) => {
                self.store.update_turn_status(
                    &turn.turn_id,
                    None,
                    ResourceStatus::Degraded,
                    true,
                )?;
                self.store.update_session_status(
                    &session.session_id,
                    ResourceStatus::Degraded,
                    Some(started.server_seq),
                )?;
                self.store.append_event(
                    &turn.resource_uri,
                    Some(&session.resource_uri),
                    "turn.degraded",
                    "singleton",
                    "remote-broker",
                    json!({ "summary": error.to_string(), "retryable": true }),
                )?;
                return Err(error);
            }
        };
        self.insert_remote_link(RemoteLinkInsert {
            host_id: session_link.host_id.clone(),
            local_resource_uri: turn.resource_uri.clone(),
            local_resource_kind: ResourceKind::Turn,
            local_id: turn.turn_id.clone(),
            remote_resource_uri: remote_reply.resource_uri,
            remote_id: remote_reply.turn_id,
            remote_cursor: 0,
        })?;
        self.store
            .update_remote_resource_cursor(&session.resource_uri, remote_reply.event_cursor)?;
        let reply = SendMessageReply {
            turn_id: turn.turn_id.clone(),
            resource_uri: turn.resource_uri.clone(),
            status: ResourceStatus::Running,
            event_cursor: started.server_seq.max(queued.server_seq),
        };
        self.record_operation_applied(
            &operation_id,
            &session_link.host_id,
            "send_message",
            Some(turn.resource_uri),
            &reply,
        )?;
        Ok(reply)
    }

    async fn close_remote_resource(
        &self,
        request: CloseResourceRequest,
    ) -> Result<CloseResourceReply> {
        let operation_id = Self::remote_operation_id(request.operation_id.clone());
        let disposition = request.disposition.clone().unwrap_or_default();
        if let Some(session_id) = request.target.session_id.clone() {
            let session = self.store.get_session(&session_id)?;
            let link = self
                .remote_link_for_resource(&session.resource_uri)?
                .ok_or_else(|| {
                    SingletonError::InvalidState(format!(
                        "session {} has no remote link",
                        session.session_id
                    ))
                })?;
            self.record_operation_pending(
                &operation_id,
                &link.host_id,
                "close_resource",
                &request,
            )?;
            let _: CloseResourceReply = self
                .call_remote(
                    &link.host_id,
                    "close_resource",
                    CloseResourceRequest {
                        target: CloseResourceTarget {
                            session_id: Some(link.remote_id),
                            workspace_id: None,
                            resource_uri: None,
                        },
                        disposition: Some(disposition.clone()),
                        force: request.force,
                        operation_id: Some(operation_id.clone()),
                    },
                )
                .await?;
            self.store.close_session(&session_id, disposition.clone())?;
            self.store.append_event(
                &session.resource_uri,
                None,
                "session.archived",
                "singleton",
                "remote-broker",
                json!({ "disposition": disposition }),
            )?;
            let reply = CloseResourceReply {
                closed: true,
                target_uri: session.resource_uri.clone(),
                cleanup_summary: CleanupSummary::default(),
            };
            self.record_operation_applied(
                &operation_id,
                &link.host_id,
                "close_resource",
                Some(session.resource_uri),
                &reply,
            )?;
            return Ok(reply);
        }

        let workspace_id = match (
            request.target.workspace_id.clone(),
            request.target.resource_uri.clone(),
        ) {
            (Some(workspace_id), _) => workspace_id,
            (None, Some(uri)) => uri
                .strip_prefix("singleton-workspace:/")
                .ok_or_else(|| {
                    SingletonError::InvalidInput(format!(
                        "close_resource only supports session_id, workspace_id, or workspace URI: {uri}"
                    ))
                })?
                .to_string(),
            (None, None) => {
                return Err(SingletonError::InvalidInput(
                    "close_resource target is required".to_string(),
                ));
            }
        };
        let workspace = self.store.get_workspace(&workspace_id)?;
        let link = self
            .remote_link_for_resource(&workspace.resource_uri)?
            .ok_or_else(|| {
                SingletonError::InvalidState(format!(
                    "workspace {} has no remote link",
                    workspace.workspace_id
                ))
            })?;
        self.record_operation_pending(&operation_id, &link.host_id, "close_resource", &request)?;
        let remote_reply: CloseResourceReply = self
            .call_remote(
                &link.host_id,
                "close_resource",
                CloseResourceRequest {
                    target: CloseResourceTarget {
                        session_id: None,
                        workspace_id: Some(link.remote_id),
                        resource_uri: None,
                    },
                    disposition: Some(disposition.clone()),
                    force: request.force,
                    operation_id: Some(operation_id.clone()),
                },
            )
            .await?;
        let status = match disposition {
            CloseDisposition::Archive => ResourceStatus::Archived,
            CloseDisposition::Dispose => ResourceStatus::Disposed,
            CloseDisposition::Delete => ResourceStatus::Deleted,
        };
        self.store
            .update_workspace_status(&workspace.workspace_id, status)?;
        self.store.append_event(
            &workspace.resource_uri,
            None,
            "workspace.closed",
            "singleton",
            "remote-broker",
            json!({ "disposition": disposition, "cleanup": remote_reply.cleanup_summary }),
        )?;
        let reply = CloseResourceReply {
            closed: true,
            target_uri: workspace.resource_uri.clone(),
            cleanup_summary: remote_reply.cleanup_summary,
        };
        self.record_operation_applied(
            &operation_id,
            &link.host_id,
            "close_resource",
            Some(workspace.resource_uri),
            &reply,
        )?;
        Ok(reply)
    }

    async fn sync_remote_session_events(
        &self,
        session_id: &str,
        wait_ms: u64,
    ) -> Option<ReadSyncStatus> {
        let session = match self.store.get_session(session_id) {
            Ok(session) => session,
            Err(_) => return None,
        };
        let link = match self.remote_link_for_resource(&session.resource_uri) {
            Ok(Some(link)) => link,
            Ok(None) => return None,
            Err(error) => {
                return Some(ReadSyncStatus {
                    freshness: ReadFreshness::StaleReconcileFailed,
                    host_id: None,
                    checked_at: Some(now_rfc3339()),
                    message: Some(error.to_string()),
                });
            }
        };
        let remote_read = self
            .call_remote::<_, ReadEventsReply>(
                &link.host_id,
                "read_events",
                ReadEventsRequest {
                    session_id: Some(link.remote_id.clone()),
                    resource_uri: None,
                    cursor: Some(link.remote_cursor),
                    limit: Some(500),
                    event_types: Vec::new(),
                    wait_ms: Some(wait_ms),
                },
            )
            .await;
        let remote_read = match remote_read {
            Ok(reply) => reply,
            Err(error) => {
                let _ = self.store.append_event(
                    &session.resource_uri,
                    None,
                    "session.degraded",
                    "singleton",
                    "remote-broker",
                    json!({
                        "host_id": link.host_id,
                        "summary": error.to_string(),
                    }),
                );
                return Some(ReadSyncStatus {
                    freshness: ReadFreshness::StaleUnavailable,
                    host_id: Some(link.host_id),
                    checked_at: Some(now_rfc3339()),
                    message: Some(error.to_string()),
                });
            }
        };
        for event in &remote_read.events {
            if let Err(error) = self.mirror_remote_event(&link.host_id, &session, event) {
                return Some(ReadSyncStatus {
                    freshness: ReadFreshness::StaleReconcileFailed,
                    host_id: Some(link.host_id),
                    checked_at: Some(now_rfc3339()),
                    message: Some(error.to_string()),
                });
            }
        }
        if let Err(error) = self
            .store
            .update_remote_resource_cursor(&session.resource_uri, remote_read.next_cursor)
        {
            return Some(ReadSyncStatus {
                freshness: ReadFreshness::StaleReconcileFailed,
                host_id: Some(link.host_id),
                checked_at: Some(now_rfc3339()),
                message: Some(error.to_string()),
            });
        }
        Some(ReadSyncStatus {
            freshness: ReadFreshness::Fresh,
            host_id: Some(link.host_id),
            checked_at: Some(now_rfc3339()),
            message: None,
        })
    }

    fn mirror_remote_event(&self, host_id: &str, session: &Session, event: &Event) -> Result<()> {
        let resource_link = self
            .store
            .get_remote_resource_link_by_remote(host_id, &event.resource_uri)?;
        let parent_link = event
            .parent_resource_uri
            .as_deref()
            .map(|parent| {
                self.store
                    .get_remote_resource_link_by_remote(host_id, parent)
            })
            .transpose()?
            .flatten();

        if event.event_type == "request.created" {
            let Some(turn_link) = parent_link else {
                return Err(SingletonError::InvalidState(format!(
                    "remote request event {} has no mapped turn parent",
                    event.event_id
                )));
            };
            if self
                .store
                .get_remote_resource_link_by_remote(host_id, &event.resource_uri)?
                .is_none()
            {
                let request_kind = match event
                    .payload
                    .get("request_kind")
                    .and_then(Value::as_str)
                    .unwrap_or("permission")
                {
                    "input" => RequestKind::Input,
                    "elicitation" => RequestKind::Elicitation,
                    _ => RequestKind::Permission,
                };
                let request = new_request(
                    session.session_id.clone(),
                    Some(turn_link.local_id.clone()),
                    request_kind,
                    backend_payload_summary(&event.payload),
                    event.payload.clone(),
                );
                self.store.insert_request(&request)?;
                self.insert_remote_link(RemoteLinkInsert {
                    host_id: host_id.to_string(),
                    local_resource_uri: request.resource_uri.clone(),
                    local_resource_kind: ResourceKind::Request,
                    local_id: request.request_id.clone(),
                    remote_resource_uri: event.resource_uri.clone(),
                    remote_id: Self::remote_resource_id(&event.resource_uri),
                    remote_cursor: 0,
                })?;
                self.store.append_event(
                    &request.resource_uri,
                    Some(&turn_link.local_resource_uri),
                    &event.event_type,
                    "remote",
                    host_id,
                    event.payload.clone(),
                )?;
            }
            return Ok(());
        }

        let local_resource_uri = resource_link
            .as_ref()
            .map(|link| link.local_resource_uri.clone())
            .unwrap_or_else(|| session.resource_uri.clone());
        let local_parent_uri = parent_link
            .as_ref()
            .map(|link| link.local_resource_uri.clone())
            .or_else(|| Some(session.resource_uri.clone()));
        let stored = self.store.append_event(
            &local_resource_uri,
            local_parent_uri.as_deref(),
            &event.event_type,
            "remote",
            host_id,
            event.payload.clone(),
        )?;

        if let Some(link) = resource_link {
            match (link.local_resource_kind, event.event_type.as_str()) {
                (ResourceKind::Turn, "turn.completed") => {
                    self.store.update_turn_status(
                        &link.local_id,
                        Some(&link.remote_id),
                        ResourceStatus::Completed,
                        true,
                    )?;
                    self.store.update_session_status(
                        &session.session_id,
                        ResourceStatus::Idle,
                        Some(stored.server_seq),
                    )?;
                }
                (ResourceKind::Turn, "turn.failed") => {
                    self.store.update_turn_status(
                        &link.local_id,
                        Some(&link.remote_id),
                        ResourceStatus::Failed,
                        true,
                    )?;
                    self.store.update_session_status(
                        &session.session_id,
                        ResourceStatus::Idle,
                        Some(stored.server_seq),
                    )?;
                }
                (ResourceKind::Turn, "turn.cancelled") => {
                    self.store.update_turn_status(
                        &link.local_id,
                        Some(&link.remote_id),
                        ResourceStatus::Cancelled,
                        true,
                    )?;
                    self.store.update_session_status(
                        &session.session_id,
                        ResourceStatus::Idle,
                        Some(stored.server_seq),
                    )?;
                }
                (ResourceKind::Turn, "turn.needs_input") => {
                    self.store.update_turn_status(
                        &link.local_id,
                        Some(&link.remote_id),
                        ResourceStatus::NeedsInput,
                        true,
                    )?;
                    self.store.update_session_status(
                        &session.session_id,
                        ResourceStatus::NeedsInput,
                        Some(stored.server_seq),
                    )?;
                }
                _ => {}
            }
        }
        Ok(())
    }

    async fn reconcile_backend_state(&self) -> Result<()> {
        let capabilities = self.backend.capabilities();
        let sessions = self.store.sessions_with_backend_session_ids()?;
        let active_turns = self.store.active_turns_for_recovery()?;
        let mut processed_turn_ids = HashSet::new();

        for session in sessions
            .into_iter()
            .filter(|session| session.backend == capabilities.backend_id)
        {
            let session_active_turns = active_turns
                .iter()
                .filter(|turn| turn.session_id == session.session_id)
                .cloned()
                .collect::<Vec<_>>();
            if let Some(backend_session_id) = session.backend_session_id.clone() {
                if capabilities.supports_resume {
                    match self
                        .backend
                        .resume_session(backend_session_id.clone())
                        .await
                    {
                        Ok(backend_session) => {
                            let reattached = self.store.append_event(
                                &session.resource_uri,
                                None,
                                "session.reattached",
                                "backend",
                                &session.backend,
                                json!({ "backend_session_id": backend_session_id }),
                            )?;
                            if session_active_turns.is_empty() {
                                self.store.update_session_status(
                                    &session.session_id,
                                    ResourceStatus::Idle,
                                    Some(reattached.server_seq),
                                )?;
                            }
                            for turn in session_active_turns {
                                processed_turn_ids.insert(turn.turn_id.clone());
                                if capabilities.supports_turn_reattach {
                                    self.reattach_active_turn(
                                        &session,
                                        &turn,
                                        backend_session.clone(),
                                    )
                                    .await?;
                                } else {
                                    self.mark_turn_interrupted_with_events(
                                        &turn,
                                        "backend resumed the session but does not support active turn reattach",
                                    )?;
                                }
                            }
                        }
                        Err(error) => {
                            self.store.append_event(
                                &session.resource_uri,
                                None,
                                "session.degraded",
                                "singleton",
                                "broker",
                                json!({
                                    "backend_session_id": backend_session_id,
                                    "summary": error.to_string(),
                                }),
                            )?;
                            self.store.update_session_status(
                                &session.session_id,
                                ResourceStatus::Degraded,
                                None,
                            )?;
                            for turn in session_active_turns {
                                processed_turn_ids.insert(turn.turn_id.clone());
                                self.mark_turn_interrupted_with_events(
                                    &turn,
                                    &format!("backend resume failed: {error}"),
                                )?;
                            }
                        }
                    }
                } else {
                    for turn in session_active_turns {
                        processed_turn_ids.insert(turn.turn_id.clone());
                        self.mark_turn_interrupted_with_events(
                            &turn,
                            "backend does not support session resume",
                        )?;
                    }
                }
            }
        }

        for turn in active_turns {
            if !processed_turn_ids.contains(&turn.turn_id) {
                self.mark_turn_interrupted_with_events(
                    &turn,
                    "backend session was unavailable during broker startup",
                )?;
            }
        }
        Ok(())
    }

    fn reconcile_interrupted_turns(&self) {
        let Ok(turns) = self.store.active_turns_for_recovery() else {
            return;
        };
        for turn in turns {
            let _ = self.mark_turn_interrupted_with_events(
                &turn,
                "turn interrupted by broker shutdown or restart",
            );
        }
    }

    async fn reattach_active_turn(
        &self,
        session: &Session,
        turn: &singleton_core::Turn,
        backend_session: BackendSession,
    ) -> Result<()> {
        let latest = Arc::new(Mutex::new(session.latest_event_cursor));
        let sink_latest = latest.clone();
        let sink_store = self.store.clone();
        let sink_session = session.clone();
        let sink_turn = turn.clone();
        let event_sink = Arc::new(move |event: BackendEvent| {
            let seq = ingest_backend_event(&sink_store, &sink_session, &sink_turn, event)?;
            if let Ok(mut latest) = sink_latest.lock() {
                *latest = seq;
            }
            Ok(())
        });
        let latest_seq = latest.lock().map(|latest| *latest).unwrap_or(0);
        match self
            .backend
            .reattach_turn(&backend_session, turn, event_sink)
            .await?
        {
            Some(backend_turn) => finalize_backend_turn(
                &self.store,
                session,
                turn,
                backend_turn,
                latest.lock().map(|latest| *latest).unwrap_or(latest_seq),
            ),
            None => self.mark_turn_interrupted_with_events(
                turn,
                "backend did not reattach the active turn",
            ),
        }
    }

    fn mark_turn_interrupted_with_events(
        &self,
        turn: &singleton_core::Turn,
        summary: &str,
    ) -> Result<()> {
        let interrupted = self.store.mark_turn_interrupted(&turn.turn_id, summary)?;
        self.cancel_pending_requests_for_turn(&interrupted, summary.to_string())?;
        let session_uri = resource_uri(ResourceKind::Session, &interrupted.session_id);
        let event = self.store.append_event(
            &interrupted.resource_uri,
            Some(&session_uri),
            "turn.failed",
            "singleton",
            "broker",
            json!({
                "summary": summary,
                "retryable": true
            }),
        )?;
        self.store.update_session_status(
            &interrupted.session_id,
            ResourceStatus::Idle,
            Some(event.server_seq),
        )?;
        Ok(())
    }

    fn cancel_pending_requests_for_turn(
        &self,
        turn: &singleton_core::Turn,
        reason: String,
    ) -> Result<Vec<PendingRequest>> {
        let requests = self
            .store
            .cancel_pending_requests_for_turn(&turn.turn_id, reason.clone())?;
        for request in &requests {
            self.store.append_event(
                &request.resource_uri,
                Some(&turn.resource_uri),
                "request.cancelled",
                "singleton",
                "broker",
                json!({
                    "request_id": request.request_id,
                    "reason": reason.clone(),
                }),
            )?;
        }
        Ok(requests)
    }

    fn spawn_backend_turn(
        &self,
        session: &Session,
        turn: &singleton_core::Turn,
        backend_session: BackendSession,
        message: String,
        mode: Option<String>,
        latest_cursor: i64,
    ) {
        let backend = self.backend.clone();
        let store = self.store.clone();
        let session = session.clone();
        let turn = turn.clone();
        let latest = Arc::new(Mutex::new(latest_cursor));
        let sink_latest = latest.clone();
        let sink_store = store.clone();
        let sink_session = session.clone();
        let sink_turn = turn.clone();
        let event_sink = Arc::new(move |event: BackendEvent| {
            let seq = ingest_backend_event(&sink_store, &sink_session, &sink_turn, event)?;
            if let Ok(mut latest) = sink_latest.lock() {
                *latest = seq;
            }
            Ok(())
        });
        tokio::spawn(async move {
            let result = backend
                .send_message(
                    &backend_session,
                    BackendMessage {
                        turn_id: turn.turn_id.clone(),
                        content: message,
                        mode,
                    },
                    event_sink,
                )
                .await;
            let latest_seq = latest.lock().map(|latest| *latest).unwrap_or(latest_cursor);
            match result {
                Ok(backend_turn) => {
                    let _ =
                        finalize_backend_turn(&store, &session, &turn, backend_turn, latest_seq);
                }
                Err(error) => {
                    let _ = fail_backend_turn(&store, &session, &turn, error.to_string());
                }
            }
        });
    }
}

fn ingest_backend_event(
    store: &Store,
    session: &Session,
    turn: &singleton_core::Turn,
    event: BackendEvent,
) -> Result<i64> {
    if event.event_type == "request.created" {
        let request_kind = match event
            .payload
            .get("request_kind")
            .and_then(Value::as_str)
            .unwrap_or("permission")
        {
            "input" => RequestKind::Input,
            "elicitation" => RequestKind::Elicitation,
            _ => RequestKind::Permission,
        };
        let summary = backend_payload_summary(&event.payload);
        let request = new_request(
            session.session_id.clone(),
            Some(turn.turn_id.clone()),
            request_kind,
            summary,
            event.payload.clone(),
        );
        store.insert_request(&request)?;
        let stored = store.append_event(
            &request.resource_uri,
            Some(&turn.resource_uri),
            &event.event_type,
            "backend",
            &session.backend,
            event.payload,
        )?;
        return Ok(stored.server_seq);
    }

    let stored = store.append_event(
        &turn.resource_uri,
        Some(&session.resource_uri),
        &event.event_type,
        "backend",
        &session.backend,
        event.payload,
    )?;
    Ok(stored.server_seq)
}

fn finalize_backend_turn(
    store: &Store,
    session: &Session,
    turn: &singleton_core::Turn,
    backend_turn: BackendTurn,
    latest_seq: i64,
) -> Result<()> {
    let mut latest = latest_seq;
    let terminal_event_type = match backend_turn.status {
        ResourceStatus::Completed => Some("turn.completed"),
        ResourceStatus::Failed => Some("turn.failed"),
        ResourceStatus::Cancelled => Some("turn.cancelled"),
        ResourceStatus::NeedsInput => Some("turn.needs_input"),
        _ => None,
    };
    let mut saw_terminal_event = false;
    for event in backend_turn.events {
        saw_terminal_event |= Some(event.event_type.as_str()) == terminal_event_type;
        latest = ingest_backend_event(store, session, turn, event)?;
    }
    if let Some(event_type) = terminal_event_type
        && !saw_terminal_event
    {
        let event = store.append_event(
            &turn.resource_uri,
            Some(&session.resource_uri),
            event_type,
            "backend",
            &session.backend,
            json!({ "backend_turn_id": backend_turn.backend_turn_id }),
        )?;
        latest = event.server_seq;
    }
    let unread = matches!(
        backend_turn.status,
        ResourceStatus::Completed
            | ResourceStatus::Failed
            | ResourceStatus::NeedsInput
            | ResourceStatus::Cancelled
    );
    store.update_turn_status(
        &turn.turn_id,
        Some(&backend_turn.backend_turn_id),
        backend_turn.status.clone(),
        unread,
    )?;
    let session_status = match backend_turn.status {
        ResourceStatus::Completed | ResourceStatus::Cancelled => ResourceStatus::Idle,
        other => other,
    };
    store.update_session_status(&session.session_id, session_status, Some(latest))?;
    Ok(())
}

fn fail_backend_turn(
    store: &Store,
    session: &Session,
    turn: &singleton_core::Turn,
    summary: String,
) -> Result<()> {
    let event = store.append_event(
        &turn.resource_uri,
        Some(&session.resource_uri),
        "turn.failed",
        "backend",
        &session.backend,
        json!({ "summary": summary, "retryable": true }),
    )?;
    store.update_turn_status(&turn.turn_id, None, ResourceStatus::Failed, true)?;
    store.update_session_status(
        &session.session_id,
        ResourceStatus::Idle,
        Some(event.server_seq),
    )?;
    Ok(())
}

fn backend_session_from(session: &Session) -> Result<BackendSession> {
    let backend_session_id = session.backend_session_id.clone().ok_or_else(|| {
        SingletonError::InvalidState(format!(
            "session {} has no backend session id",
            session.session_id
        ))
    })?;
    Ok(BackendSession {
        backend_id: session.backend.clone(),
        backend_session_id,
        status: session.status.clone(),
    })
}

fn no_turn_latest_output(session: &Session) -> LatestOutput {
    LatestOutput {
        session_id: session.session_id.clone(),
        turn_id: None,
        turn_resource_uri: None,
        status: None,
        event_cursor: session.latest_event_cursor,
        source_event: None,
        result_text: None,
        result_source: LatestOutputSource::None,
        needs_event_inspection: false,
        inspection_hint: Some(
            "no completed, failed, or cancelled turn exists for this session".to_string(),
        ),
        sync_status: None,
    }
}

fn latest_output_from_events(session: &Session, turn: &Turn, events: &[Event]) -> LatestOutput {
    let latest_event = events.last().map(latest_output_event_ref);
    let event_cursor = latest_event
        .as_ref()
        .map(|event| event.server_seq)
        .unwrap_or(session.latest_event_cursor);
    if let Some(output) = extract_latest_result(events) {
        return LatestOutput {
            session_id: session.session_id.clone(),
            turn_id: Some(turn.turn_id.clone()),
            turn_resource_uri: Some(turn.resource_uri.clone()),
            status: Some(turn.status.clone()),
            event_cursor,
            source_event: Some(output.event),
            result_text: Some(output.text),
            result_source: output.source,
            needs_event_inspection: false,
            inspection_hint: None,
            sync_status: None,
        };
    }

    LatestOutput {
        session_id: session.session_id.clone(),
        turn_id: Some(turn.turn_id.clone()),
        turn_resource_uri: Some(turn.resource_uri.clone()),
        status: Some(turn.status.clone()),
        event_cursor,
        source_event: latest_event,
        result_text: None,
        result_source: LatestOutputSource::None,
        needs_event_inspection: true,
        inspection_hint: Some(format!(
            "no concise assistant text, terminal summary, or error message found in {} recent turn event(s); inspect raw events with read_events(resource_uri={})",
            events.len(),
            turn.resource_uri
        )),
        sync_status: None,
    }
}

struct ExtractedOutput {
    text: String,
    source: LatestOutputSource,
    event: LatestOutputEventRef,
}

fn extract_latest_result(events: &[Event]) -> Option<ExtractedOutput> {
    latest_assistant_message(events)
        .or_else(|| latest_turn_summary(events))
        .or_else(|| latest_error_message(events))
}

fn latest_assistant_message(events: &[Event]) -> Option<ExtractedOutput> {
    events
        .iter()
        .rev()
        .filter(|event| event.event_type == "assistant.message")
        .find_map(|event| {
            assistant_message_text(event).map(|text| ExtractedOutput {
                text,
                source: LatestOutputSource::AssistantMessage,
                event: latest_output_event_ref(event),
            })
        })
}

fn latest_turn_summary(events: &[Event]) -> Option<ExtractedOutput> {
    events
        .iter()
        .rev()
        .filter(|event| {
            matches!(
                event.event_type.as_str(),
                "turn.completed" | "turn.failed" | "turn.cancelled" | "message.completed"
            )
        })
        .find_map(|event| {
            terminal_summary_text(event).map(|text| ExtractedOutput {
                text,
                source: LatestOutputSource::TurnSummary,
                event: latest_output_event_ref(event),
            })
        })
}

fn latest_error_message(events: &[Event]) -> Option<ExtractedOutput> {
    events
        .iter()
        .rev()
        .filter(|event| event.event_type == "session.error")
        .find_map(|event| {
            error_message_text(event).map(|text| ExtractedOutput {
                text,
                source: LatestOutputSource::ErrorMessage,
                event: latest_output_event_ref(event),
            })
        })
}

fn assistant_message_text(event: &Event) -> Option<String> {
    string_path(&event.payload, &["data", "content"])
        .or_else(|| string_path(&event.payload, &["content"]))
        .map(ToString::to_string)
}

fn terminal_summary_text(event: &Event) -> Option<String> {
    string_path(&event.payload, &["summary"])
        .or_else(|| string_path(&event.payload, &["content"]))
        .map(ToString::to_string)
}

fn error_message_text(event: &Event) -> Option<String> {
    string_path(&event.payload, &["data", "message"])
        .or_else(|| string_path(&event.payload, &["message"]))
        .or_else(|| string_path(&event.payload, &["summary"]))
        .map(ToString::to_string)
}

fn string_path<'a>(value: &'a Value, path: &[&str]) -> Option<&'a str> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    current.as_str().filter(|text| !text.trim().is_empty())
}

fn session_id_from_resource_uri(resource_uri_value: Option<&str>) -> Option<String> {
    resource_uri_value?
        .strip_prefix("singleton-session:/")
        .map(ToString::to_string)
}

fn latest_output_event_ref(event: &Event) -> LatestOutputEventRef {
    LatestOutputEventRef {
        event_id: event.event_id.clone(),
        server_seq: event.server_seq,
        event_type: event.event_type.clone(),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct EnsureWorkspaceRequest {
    pub spec: WorkspaceSpec,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CreateSessionRequest {
    pub description: String,
    pub title: Option<String>,
    pub backend: Option<String>,
    pub workspace: Option<WorkspaceSpec>,
    pub model: Option<String>,
    pub mode: Option<String>,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CreateSessionReply {
    pub session_id: String,
    pub resource_uri: String,
    pub workspace_id: Option<String>,
    pub status: ResourceStatus,
    pub event_cursor: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SendMessageRequest {
    pub session_id: String,
    pub message: String,
    pub mode: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SendMessageReply {
    pub turn_id: String,
    pub resource_uri: String,
    pub status: ResourceStatus,
    pub event_cursor: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
pub struct ReadEventsRequest {
    pub session_id: Option<String>,
    pub resource_uri: Option<String>,
    pub cursor: Option<i64>,
    pub limit: Option<usize>,
    #[serde(default)]
    pub event_types: Vec<String>,
    pub wait_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ReadEventsReply {
    pub events: Vec<Event>,
    pub next_cursor: i64,
    pub timed_out: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync_status: Option<ReadSyncStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GetLatestOutputRequest {
    pub session_id: String,
    pub turn_id: Option<TurnId>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct SessionDetail {
    pub session: Session,
    pub workspace: Option<Workspace>,
    pub active_turn: Option<singleton_core::Turn>,
    pub pending_requests: Vec<PendingRequest>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ResolveRequest {
    pub request_id: String,
    pub decision: RequestDecision,
    pub response: Option<Value>,
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
pub struct AckInboxRequest {
    pub session_id: Option<String>,
    pub turn_id: Option<TurnId>,
    #[serde(default)]
    pub all: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct AckInboxReply {
    pub acknowledged: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CancelTurnRequest {
    pub session_id: String,
    pub turn_id: Option<TurnId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CancelTurnReply {
    pub cancelled: bool,
    pub turn_id: TurnId,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema, Default)]
pub struct CloseResourceTarget {
    pub session_id: Option<String>,
    pub workspace_id: Option<String>,
    pub resource_uri: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CloseResourceRequest {
    pub target: CloseResourceTarget,
    pub disposition: Option<CloseDisposition>,
    #[serde(default)]
    pub force: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct CloseResourceReply {
    pub closed: bool,
    pub target_uri: String,
    pub cleanup_summary: CleanupSummary,
}

pub fn ahp_like_session_snapshot(detail: &SessionDetail) -> Value {
    json!({
        "resource": detail.session.resource_uri,
        "kind": "session",
        "status": detail.session.status,
        "workspace": detail.workspace.as_ref().map(|workspace| compact_json(&json!(workspace))),
        "activeTurn": detail.active_turn.as_ref().map(|turn| turn.resource_uri.clone()),
        "pendingRequests": detail
            .pending_requests
            .iter()
            .map(|request| request.resource_uri.clone())
            .collect::<Vec<_>>(),
        "cursor": detail.session.latest_event_cursor,
    })
}

#[cfg(test)]
mod tests {
    use async_trait::async_trait;
    use singleton_core::{
        CapabilityDefaults, CleanupPolicy, Host, HostCapabilities, HostKind, LOCAL_HOST_ID,
        LatestOutputSource, RemoteHostHealth, WorkspaceSpec,
    };
    use singleton_host::LocalHostConnector;
    use singleton_test_support::{FakeBackend, FakeTurnBehavior};
    use tempfile::TempDir;

    use super::*;

    #[derive(Clone)]
    struct InProcessRemoteBroker {
        host: Host,
        broker: Broker<FakeBackend, LocalHostConnector>,
    }

    impl InProcessRemoteBroker {
        fn new(host_id: &str, backend: FakeBackend) -> Result<Self> {
            Ok(Self {
                host: Host {
                    host_id: host_id.to_string(),
                    resource_uri: resource_uri(ResourceKind::Host, host_id),
                    kind: HostKind::Ssh,
                    status: ResourceStatus::Ready,
                    capabilities: HostCapabilities {
                        workspace_providers: vec![
                            "local_path".to_string(),
                            "git_worktree".to_string(),
                            "backend_default".to_string(),
                        ],
                        agent_backends: vec![singleton_core::FAKE_BACKEND_ID.to_string()],
                        supports_reconnect: true,
                        supports_ordered_events: true,
                    },
                },
                broker: Broker::new(Store::open_memory()?, backend, LocalHostConnector),
            })
        }
    }

    #[async_trait]
    impl RemoteBrokerRegistry for InProcessRemoteBroker {
        fn hosts(&self) -> Vec<Host> {
            vec![self.host.clone()]
        }

        fn cached_health(&self, _host_id: &str) -> Option<RemoteHostHealth> {
            None
        }

        async fn call_tool(
            &self,
            _host_id: &str,
            tool_name: &str,
            arguments: Value,
        ) -> Result<Value> {
            match tool_name {
                "ensure_workspace" => {
                    let request: EnsureWorkspaceRequest = serde_json::from_value(arguments)
                        .map_err(|error| SingletonError::InvalidInput(error.to_string()))?;
                    test_remote_value(self.broker.ensure_workspace_request(request).await?)
                }
                "create_session" => {
                    let request: CreateSessionRequest = serde_json::from_value(arguments)
                        .map_err(|error| SingletonError::InvalidInput(error.to_string()))?;
                    test_remote_value(self.broker.create_session(request).await?)
                }
                "send_message" => {
                    let request: SendMessageRequest = serde_json::from_value(arguments)
                        .map_err(|error| SingletonError::InvalidInput(error.to_string()))?;
                    test_remote_value(self.broker.send_message(request).await?)
                }
                "read_events" => {
                    let request: ReadEventsRequest = serde_json::from_value(arguments)
                        .map_err(|error| SingletonError::InvalidInput(error.to_string()))?;
                    test_remote_value(self.broker.read_events(request).await?)
                }
                "get_session" => {
                    let request: GetSessionRequestForTest = serde_json::from_value(arguments)
                        .map_err(|error| SingletonError::InvalidInput(error.to_string()))?;
                    test_remote_value(self.broker.get_session(&request.session_id)?)
                }
                "resolve_request" => {
                    let request: ResolveRequest = serde_json::from_value(arguments)
                        .map_err(|error| SingletonError::InvalidInput(error.to_string()))?;
                    test_remote_value(self.broker.resolve_request(request).await?)
                }
                "cancel_turn" => {
                    let request: CancelTurnRequest = serde_json::from_value(arguments)
                        .map_err(|error| SingletonError::InvalidInput(error.to_string()))?;
                    test_remote_value(self.broker.cancel_turn(request).await?)
                }
                "close_resource" => {
                    let request: CloseResourceRequest = serde_json::from_value(arguments)
                        .map_err(|error| SingletonError::InvalidInput(error.to_string()))?;
                    test_remote_value(self.broker.close_resource(request).await?)
                }
                other => Err(SingletonError::InvalidInput(format!(
                    "unsupported test remote tool {other}"
                ))),
            }
        }
    }

    fn test_remote_value<T: Serialize>(value: T) -> Result<Value> {
        serde_json::to_value(value).map_err(|error| {
            SingletonError::InvalidState(format!("serialize remote test result: {error}"))
        })
    }

    #[derive(Deserialize)]
    struct GetSessionRequestForTest {
        session_id: String,
    }

    #[test]
    fn capability_defaults_do_not_advertise_unavailable_host() -> Result<()> {
        let broker = Broker::new(
            Store::open_memory()?,
            FakeBackend::new(),
            LocalHostConnector,
        )
        .with_capability_defaults(
            "default",
            CapabilityDefaults {
                default_host: "devbox".to_string(),
                ..CapabilityDefaults::default()
            },
        );

        let capabilities = broker.get_capabilities();

        assert_eq!(capabilities.defaults.default_host, LOCAL_HOST_ID);
        assert_eq!(capabilities.hosts.len(), 1);
        assert_eq!(capabilities.hosts[0].host_id, LOCAL_HOST_ID);
        Ok(())
    }

    #[tokio::test]
    async fn remote_broker_forwards_turns_and_mirrors_events() -> Result<()> {
        let remote = InProcessRemoteBroker::new(
            "devbox",
            FakeBackend::with_behaviors([FakeTurnBehavior::Complete {
                summary: "remote turn completed".to_string(),
            }]),
        )?;
        let broker = Broker::new(
            Store::open_memory()?,
            FakeBackend::new(),
            LocalHostConnector,
        )
        .with_capability_defaults(
            "default",
            CapabilityDefaults {
                default_host: "devbox".to_string(),
                ..CapabilityDefaults::default()
            },
        )
        .with_remote_registry(Arc::new(remote));

        let capabilities = broker.get_capabilities();
        assert_eq!(capabilities.defaults.default_host, "devbox");
        assert!(
            capabilities
                .hosts
                .iter()
                .any(|host| host.host_id == "devbox" && host.kind == HostKind::Ssh)
        );

        let created = broker
            .create_session(CreateSessionRequest {
                description: "Remote session".to_string(),
                title: None,
                backend: None,
                workspace: None,
                model: None,
                mode: None,
                labels: Vec::new(),
                operation_id: Some("op_remote_create".to_string()),
            })
            .await?;
        let sent = broker
            .send_message(SendMessageRequest {
                session_id: created.session_id.clone(),
                message: "hello remote".to_string(),
                mode: None,
                operation_id: Some("op_remote_send".to_string()),
            })
            .await?;
        let events = broker
            .read_events(ReadEventsRequest {
                session_id: Some(created.session_id.clone()),
                cursor: Some(sent.event_cursor),
                limit: Some(100),
                event_types: vec!["turn.completed".to_string()],
                wait_ms: Some(1_000),
                resource_uri: None,
            })
            .await?;

        assert_eq!(
            events.sync_status.as_ref().map(|status| &status.freshness),
            Some(&ReadFreshness::Fresh)
        );
        assert!(
            events
                .events
                .iter()
                .any(|event| event.event_type == "turn.completed")
        );
        let latest = broker
            .get_latest_output(GetLatestOutputRequest {
                session_id: created.session_id,
                turn_id: Some(sent.turn_id),
            })
            .await?;
        assert_eq!(latest.result_text.as_deref(), Some("remote turn completed"));
        assert_eq!(latest.result_source, LatestOutputSource::TurnSummary);
        Ok(())
    }

    #[tokio::test]
    async fn create_send_and_read_events_with_fake_backend() -> Result<()> {
        let temp = TempDir::new()
            .map_err(|error| SingletonError::Store(format!("create temp dir: {error}")))?;
        let broker = Broker::new(
            Store::open_memory()?,
            FakeBackend::new(),
            LocalHostConnector,
        );
        let created = broker
            .create_session(CreateSessionRequest {
                description: "Test session".to_string(),
                title: None,
                backend: None,
                workspace: Some(WorkspaceSpec::LocalPath {
                    path: temp.path().to_string_lossy().to_string(),
                    host_id: None,
                    cleanup_policy: Some(CleanupPolicy::Keep),
                }),
                model: None,
                mode: None,
                labels: vec!["test".to_string()],
                operation_id: None,
            })
            .await?;
        let sent = broker
            .send_message(SendMessageRequest {
                session_id: created.session_id.clone(),
                message: "hello".to_string(),
                mode: None,
                operation_id: None,
            })
            .await?;

        assert_eq!(sent.status, ResourceStatus::Running);
        let events = broker
            .read_events(ReadEventsRequest {
                session_id: Some(created.session_id),
                cursor: Some(sent.event_cursor),
                limit: Some(100),
                event_types: vec!["turn.completed".to_string()],
                wait_ms: Some(1_000),
                resource_uri: None,
            })
            .await?;
        assert!(
            events
                .events
                .iter()
                .any(|event| event.event_type == "turn.completed")
        );
        assert_eq!(events.events.len(), 1);
        assert_eq!(broker.get_inbox()?.counts.completed_turn, 1);
        Ok(())
    }

    #[tokio::test]
    async fn latest_output_returns_fake_completion_summary() -> Result<()> {
        let broker = Broker::new(
            Store::open_memory()?,
            FakeBackend::with_behaviors([FakeTurnBehavior::Complete {
                summary: "finished compactly".to_string(),
            }]),
            LocalHostConnector,
        );
        let created = create_basic_session(&broker, "Latest output success").await?;
        let sent = broker
            .send_message(SendMessageRequest {
                session_id: created.session_id.clone(),
                message: "finish".to_string(),
                mode: None,
                operation_id: None,
            })
            .await?;
        broker
            .read_events(ReadEventsRequest {
                session_id: Some(created.session_id.clone()),
                cursor: Some(sent.event_cursor),
                limit: Some(100),
                event_types: vec!["turn.completed".to_string()],
                wait_ms: Some(1_000),
                resource_uri: None,
            })
            .await?;

        let output = broker
            .get_latest_output(GetLatestOutputRequest {
                session_id: created.session_id,
                turn_id: None,
            })
            .await?;

        assert_eq!(output.turn_id.as_deref(), Some(sent.turn_id.as_str()));
        assert_eq!(output.status, Some(ResourceStatus::Completed));
        assert_eq!(output.result_text.as_deref(), Some("finished compactly"));
        assert_eq!(output.result_source, LatestOutputSource::TurnSummary);
        assert!(!output.needs_event_inspection);
        assert_eq!(
            output
                .source_event
                .as_ref()
                .map(|event| event.event_type.as_str()),
            Some("turn.completed")
        );
        Ok(())
    }

    #[tokio::test]
    async fn latest_output_returns_fake_failure_summary() -> Result<()> {
        let broker = Broker::new(
            Store::open_memory()?,
            FakeBackend::with_behaviors([FakeTurnBehavior::Fail {
                summary: "backend failed deterministically".to_string(),
            }]),
            LocalHostConnector,
        );
        let created = create_basic_session(&broker, "Latest output failure").await?;
        let sent = broker
            .send_message(SendMessageRequest {
                session_id: created.session_id.clone(),
                message: "fail".to_string(),
                mode: None,
                operation_id: None,
            })
            .await?;
        broker
            .read_events(ReadEventsRequest {
                session_id: Some(created.session_id.clone()),
                cursor: Some(sent.event_cursor),
                limit: Some(100),
                event_types: vec!["turn.failed".to_string()],
                wait_ms: Some(1_000),
                resource_uri: None,
            })
            .await?;

        let output = broker
            .get_latest_output(GetLatestOutputRequest {
                session_id: created.session_id,
                turn_id: Some(sent.turn_id),
            })
            .await?;

        assert_eq!(output.status, Some(ResourceStatus::Failed));
        assert_eq!(
            output.result_text.as_deref(),
            Some("backend failed deterministically")
        );
        assert_eq!(output.result_source, LatestOutputSource::TurnSummary);
        assert!(!output.needs_event_inspection);
        Ok(())
    }

    #[tokio::test]
    async fn latest_output_marks_completed_turn_without_text_for_event_inspection() -> Result<()> {
        let broker = Broker::new(
            Store::open_memory()?,
            FakeBackend::with_behaviors([FakeTurnBehavior::CompleteWithoutOutput]),
            LocalHostConnector,
        );
        let created = create_basic_session(&broker, "Latest output no text").await?;
        let sent = broker
            .send_message(SendMessageRequest {
                session_id: created.session_id.clone(),
                message: "finish quietly".to_string(),
                mode: None,
                operation_id: None,
            })
            .await?;
        broker
            .read_events(ReadEventsRequest {
                session_id: Some(created.session_id.clone()),
                cursor: Some(sent.event_cursor),
                limit: Some(100),
                event_types: vec!["turn.completed".to_string()],
                wait_ms: Some(1_000),
                resource_uri: None,
            })
            .await?;

        let output = broker
            .get_latest_output(GetLatestOutputRequest {
                session_id: created.session_id,
                turn_id: None,
            })
            .await?;

        assert_eq!(output.status, Some(ResourceStatus::Completed));
        assert_eq!(output.result_text, None);
        assert_eq!(output.result_source, LatestOutputSource::None);
        assert!(output.needs_event_inspection);
        assert!(output.event_cursor >= sent.event_cursor);
        assert_eq!(
            output
                .source_event
                .as_ref()
                .map(|event| event.event_type.as_str()),
            Some("turn.completed")
        );
        Ok(())
    }

    #[tokio::test]
    async fn latest_output_returns_no_turn_metadata_for_empty_session() -> Result<()> {
        let broker = Broker::new(
            Store::open_memory()?,
            FakeBackend::new(),
            LocalHostConnector,
        );
        let created = create_basic_session(&broker, "Latest output no turn").await?;

        let output = broker
            .get_latest_output(GetLatestOutputRequest {
                session_id: created.session_id.clone(),
                turn_id: None,
            })
            .await?;

        assert_eq!(output.session_id, created.session_id);
        assert_eq!(output.turn_id, None);
        assert_eq!(output.status, None);
        assert_eq!(output.result_text, None);
        assert_eq!(output.result_source, LatestOutputSource::None);
        assert!(!output.needs_event_inspection);
        assert_eq!(output.event_cursor, created.event_cursor);
        Ok(())
    }

    #[tokio::test]
    async fn permission_request_flows_to_inbox_and_resolves() -> Result<()> {
        let broker = Broker::new(
            Store::open_memory()?,
            FakeBackend::with_behaviors([FakeTurnBehavior::RequestPermission {
                summary: "Allow command?".to_string(),
            }]),
            LocalHostConnector,
        );
        let created = broker
            .create_session(CreateSessionRequest {
                description: "Needs permission".to_string(),
                title: None,
                backend: None,
                workspace: None,
                model: None,
                mode: None,
                labels: Vec::new(),
                operation_id: None,
            })
            .await?;
        broker
            .send_message(SendMessageRequest {
                session_id: created.session_id,
                message: "run command".to_string(),
                mode: None,
                operation_id: None,
            })
            .await?;
        broker
            .read_events(ReadEventsRequest {
                session_id: None,
                cursor: Some(0),
                limit: Some(100),
                event_types: vec!["request.created".to_string()],
                wait_ms: Some(1_000),
                resource_uri: None,
            })
            .await?;
        let inbox = broker.get_inbox()?;
        assert_eq!(inbox.counts.permission_request, 1);
        let request_id = match &inbox.items[0] {
            InboxItem::PermissionRequest { request_id, .. } => request_id.clone(),
            _ => {
                return Err(SingletonError::InvalidState(
                    "expected permission request inbox item".to_string(),
                ));
            }
        };
        let resolved = broker
            .resolve_request(ResolveRequest {
                request_id,
                decision: RequestDecision::Approve,
                response: Some(json!({ "scope": "once" })),
                reason: None,
                operation_id: None,
            })
            .await?;
        assert_eq!(resolved.status, singleton_core::RequestStatus::Resolved);
        Ok(())
    }

    #[tokio::test]
    async fn workspace_delete_refuses_active_session() -> Result<()> {
        let temp = TempDir::new()
            .map_err(|error| SingletonError::Store(format!("create temp dir: {error}")))?;
        let broker = Broker::new(
            Store::open_memory()?,
            FakeBackend::new(),
            LocalHostConnector,
        );
        let created = broker
            .create_session(CreateSessionRequest {
                description: "Uses workspace".to_string(),
                title: None,
                backend: None,
                workspace: Some(WorkspaceSpec::LocalPath {
                    path: temp.path().to_string_lossy().to_string(),
                    host_id: None,
                    cleanup_policy: None,
                }),
                model: None,
                mode: None,
                labels: Vec::new(),
                operation_id: None,
            })
            .await?;

        let err = broker
            .close_resource(CloseResourceRequest {
                target: CloseResourceTarget {
                    session_id: None,
                    workspace_id: created.workspace_id,
                    resource_uri: None,
                },
                disposition: Some(CloseDisposition::Delete),
                force: false,
                operation_id: None,
            })
            .await;
        assert!(err.is_err());
        Ok(())
    }

    #[tokio::test]
    async fn ahp_like_snapshot_uses_resource_links() -> Result<()> {
        let broker = Broker::new(
            Store::open_memory()?,
            FakeBackend::new(),
            LocalHostConnector,
        );
        let created = broker
            .create_session(CreateSessionRequest {
                description: "Snapshot".to_string(),
                title: None,
                backend: None,
                workspace: None,
                model: None,
                mode: None,
                labels: Vec::new(),
                operation_id: None,
            })
            .await?;
        let detail = broker.get_session(&created.session_id)?;
        let snapshot = ahp_like_session_snapshot(&detail);
        assert_eq!(snapshot["kind"], "session");
        assert_eq!(snapshot["resource"], created.resource_uri);
        Ok(())
    }

    #[test]
    fn broker_startup_marks_stale_active_turns_interrupted() -> Result<()> {
        let store = Store::open_memory()?;
        let mut session = new_session("interrupted".to_string(), "fake".to_string(), None);
        session.status = ResourceStatus::Running;
        session.backend_session_id = Some("fake_sess_existing".to_string());
        store.insert_session(&session)?;
        let mut turn = new_turn(session.session_id.clone(), "still running".to_string());
        turn.status = ResourceStatus::Running;
        store.insert_turn(&turn)?;

        let broker = Broker::new(store.clone(), FakeBackend::new(), LocalHostConnector);

        let recovered_turn = store.get_turn(&turn.turn_id)?;
        assert_eq!(recovered_turn.status, ResourceStatus::Failed);
        assert!(recovered_turn.unread);
        let recovered_session = broker.get_session(&session.session_id)?;
        assert_eq!(recovered_session.session.status, ResourceStatus::Idle);
        let events = broker.store().read_events(
            Some(&turn.resource_uri),
            0,
            100,
            &["turn.failed".to_string()],
        )?;
        assert_eq!(events.len(), 1);
        Ok(())
    }

    #[tokio::test]
    async fn ack_inbox_marks_completed_turns_read() -> Result<()> {
        let broker = Broker::new(
            Store::open_memory()?,
            FakeBackend::new(),
            LocalHostConnector,
        );
        let created = broker
            .create_session(CreateSessionRequest {
                description: "Ack inbox".to_string(),
                title: None,
                backend: None,
                workspace: None,
                model: None,
                mode: None,
                labels: Vec::new(),
                operation_id: None,
            })
            .await?;
        let sent = broker
            .send_message(SendMessageRequest {
                session_id: created.session_id,
                message: "finish".to_string(),
                mode: None,
                operation_id: None,
            })
            .await?;
        broker
            .read_events(ReadEventsRequest {
                session_id: None,
                resource_uri: None,
                cursor: Some(sent.event_cursor),
                limit: Some(100),
                event_types: vec!["turn.completed".to_string()],
                wait_ms: Some(1_000),
            })
            .await?;
        assert_eq!(broker.get_inbox()?.counts.completed_turn, 1);

        let reply = broker.ack_inbox(AckInboxRequest {
            turn_id: Some(sent.turn_id),
            ..AckInboxRequest::default()
        })?;

        assert_eq!(reply.acknowledged, 1);
        assert_eq!(broker.get_inbox()?.counts.completed_turn, 0);
        Ok(())
    }

    #[tokio::test]
    async fn cancel_turn_cancels_pending_requests() -> Result<()> {
        let broker = Broker::new(
            Store::open_memory()?,
            FakeBackend::with_behaviors([FakeTurnBehavior::RequestPermission {
                summary: "Allow command?".to_string(),
            }]),
            LocalHostConnector,
        );
        let created = broker
            .create_session(CreateSessionRequest {
                description: "Cancel request".to_string(),
                title: None,
                backend: None,
                workspace: None,
                model: None,
                mode: None,
                labels: Vec::new(),
                operation_id: None,
            })
            .await?;
        let sent = broker
            .send_message(SendMessageRequest {
                session_id: created.session_id.clone(),
                message: "needs permission".to_string(),
                mode: None,
                operation_id: None,
            })
            .await?;
        broker
            .read_events(ReadEventsRequest {
                session_id: Some(created.session_id.clone()),
                cursor: Some(sent.event_cursor),
                limit: Some(100),
                event_types: vec!["request.created".to_string()],
                wait_ms: Some(1_000),
                resource_uri: None,
            })
            .await?;
        assert_eq!(broker.get_inbox()?.counts.permission_request, 1);

        broker
            .cancel_turn(CancelTurnRequest {
                session_id: created.session_id,
                turn_id: Some(sent.turn_id),
                operation_id: None,
            })
            .await?;

        assert_eq!(broker.get_inbox()?.counts.permission_request, 0);
        assert!(
            broker
                .store()
                .read_events(None, 0, 100, &["request.cancelled".to_string()])?
                .iter()
                .any(|event| event.event_type == "request.cancelled")
        );
        Ok(())
    }

    #[tokio::test]
    async fn broker_startup_reattaches_active_turn_when_backend_supports_it() -> Result<()> {
        let store = Store::open_memory()?;
        let mut session = new_session("reattach".to_string(), "fake".to_string(), None);
        session.status = ResourceStatus::Running;
        session.backend_session_id = Some("fake_sess_existing".to_string());
        store.insert_session(&session)?;
        let mut turn = new_turn(session.session_id.clone(), "still running".to_string());
        turn.status = ResourceStatus::Running;
        turn.backend_turn_id = Some("fake_turn_existing".to_string());
        store.insert_turn(&turn)?;

        let broker =
            Broker::new_with_reconnect(store.clone(), FakeBackend::new(), LocalHostConnector)
                .await?;

        let recovered_turn = store.get_turn(&turn.turn_id)?;
        assert_eq!(recovered_turn.status, ResourceStatus::Completed);
        assert!(recovered_turn.unread);
        let recovered_session = broker.get_session(&session.session_id)?;
        assert_eq!(recovered_session.session.status, ResourceStatus::Idle);
        let events = broker.store().read_events(
            Some(&turn.resource_uri),
            0,
            100,
            &["turn.reattached".to_string(), "turn.completed".to_string()],
        )?;
        assert_eq!(events.len(), 2);
        Ok(())
    }

    async fn create_basic_session(
        broker: &Broker<FakeBackend, LocalHostConnector>,
        description: &str,
    ) -> Result<CreateSessionReply> {
        broker
            .create_session(CreateSessionRequest {
                description: description.to_string(),
                title: None,
                backend: None,
                workspace: None,
                model: None,
                mode: None,
                labels: Vec::new(),
                operation_id: None,
            })
            .await
    }
}
