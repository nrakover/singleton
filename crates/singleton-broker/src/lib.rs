use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use schemars::JsonSchema;
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
use singleton_core::{HostConnector, ReadSyncStatus, compact_json};
use singleton_store::{Store, new_request, new_session, new_turn};

pub struct Broker<B, H>
where
    B: AgentBackend + 'static,
    H: HostConnector + 'static,
{
    store: Store,
    backend: Arc<B>,
    host: Arc<H>,
    default_profile: String,
    defaults: CapabilityDefaults,
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

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn get_capabilities(&self) -> Capabilities {
        let hosts = vec![self.host.host()];
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
        let workspace = match spec {
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
                    sync_status: None,
                });
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }

    pub fn get_latest_output(&self, request: GetLatestOutputRequest) -> Result<LatestOutput> {
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
            return Ok(no_turn_latest_output(&session));
        };
        let events = self
            .store
            .read_recent_events_for_resource(&turn.resource_uri, 500)?;
        Ok(latest_output_from_events(&session, &turn, &events))
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

    pub fn resolve_request(&self, request: ResolveRequest) -> Result<PendingRequest> {
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
        Ok(resolved)
    }

    pub async fn cancel_turn(&self, request: CancelTurnRequest) -> Result<CancelTurnReply> {
        let session = self.store.get_session(&request.session_id)?;
        let turn = match request.turn_id {
            Some(turn_id) => self.store.get_turn(&turn_id)?,
            None => self
                .store
                .active_turn_for_session(&session.session_id)?
                .ok_or_else(|| SingletonError::InvalidState("session has no active turn".into()))?,
        };
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

fn latest_output_event_ref(event: &Event) -> LatestOutputEventRef {
    LatestOutputEventRef {
        event_id: event.event_id.clone(),
        server_seq: event.server_seq,
        event_type: event.event_type.clone(),
    }
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
    use singleton_core::{
        CapabilityDefaults, CleanupPolicy, LOCAL_HOST_ID, LatestOutputSource, WorkspaceSpec,
    };
    use singleton_host::LocalHostConnector;
    use singleton_test_support::{FakeBackend, FakeTurnBehavior};
    use tempfile::TempDir;

    use super::*;

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

        let output = broker.get_latest_output(GetLatestOutputRequest {
            session_id: created.session_id,
            turn_id: None,
        })?;

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

        let output = broker.get_latest_output(GetLatestOutputRequest {
            session_id: created.session_id,
            turn_id: Some(sent.turn_id),
        })?;

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

        let output = broker.get_latest_output(GetLatestOutputRequest {
            session_id: created.session_id,
            turn_id: None,
        })?;

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

        let output = broker.get_latest_output(GetLatestOutputRequest {
            session_id: created.session_id.clone(),
            turn_id: None,
        })?;

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
        let resolved = broker.resolve_request(ResolveRequest {
            request_id,
            decision: RequestDecision::Approve,
            response: Some(json!({ "scope": "once" })),
            reason: None,
            operation_id: None,
        })?;
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
