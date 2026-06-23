use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use github_copilot_sdk::handler::{
    ElicitationHandler, PermissionHandler, PermissionResult, UserInputHandler, UserInputResponse,
};
use github_copilot_sdk::session::Session as SdkSession;
use github_copilot_sdk::subscription::RecvErrorKind;
use github_copilot_sdk::types::{
    ElicitationRequest, ElicitationResult, MessageOptions, PermissionRequestData,
    RequestId as SdkRequestId, ResumeSessionConfig, SessionConfig, SessionEvent,
    SessionId as SdkSessionId,
};
use github_copilot_sdk::{Client, ClientOptions};
use serde_json::{Value, json};
use singleton_core::{
    AgentBackend, BackendCapabilities, BackendEvent, BackendEventSink, BackendMessage,
    BackendSession, BackendSessionConfig, BackendSessionId, BackendTurn, BackendTurnId,
    COPILOT_BACKEND_ID, PendingRequest, RequestDecision, RequestKind, RequestStatus,
    ResourceStatus, Result, SingletonError, backend_payload_summary, new_id,
};
use singleton_store::{Store, new_request};
use tokio::sync::Mutex;
use tokio::time::{sleep, timeout};

#[derive(Clone)]
pub struct CopilotBackend {
    working_directory: PathBuf,
    client: Arc<Mutex<Option<Client>>>,
    sessions: Arc<Mutex<HashMap<BackendSessionId, Arc<SdkSession>>>>,
    request_broker: Option<Arc<StoreRequestBroker>>,
}

impl CopilotBackend {
    pub fn new(working_directory: PathBuf) -> Self {
        Self {
            working_directory,
            client: Arc::new(Mutex::new(None)),
            sessions: Arc::new(Mutex::new(HashMap::new())),
            request_broker: None,
        }
    }

    pub fn with_request_store(mut self, store: Store) -> Self {
        self.request_broker = Some(Arc::new(StoreRequestBroker::new(store)));
        self
    }

    async fn client(&self) -> Result<Client> {
        if let Some(client) = self.client.lock().await.as_ref().cloned() {
            return Ok(client);
        }
        let mut options = ClientOptions::default();
        options.working_directory = self.working_directory.clone();
        let client = Client::start(options).await.map_err(copilot_err)?;
        let mut guard = self.client.lock().await;
        if guard.is_none() {
            *guard = Some(client.clone());
        }
        Ok(client)
    }

    async fn session(&self, backend_session_id: &str) -> Result<Arc<SdkSession>> {
        self.sessions
            .lock()
            .await
            .get(backend_session_id)
            .cloned()
            .ok_or_else(|| SingletonError::Backend {
                backend: COPILOT_BACKEND_ID.to_string(),
                message: format!("Copilot SDK session is not attached: {backend_session_id}"),
            })
    }

    fn configure_session(&self, mut config: SessionConfig) -> SessionConfig {
        if let Some(handler) = &self.request_broker {
            let permission_handler: Arc<dyn PermissionHandler> = handler.clone();
            let elicitation_handler: Arc<dyn ElicitationHandler> = handler.clone();
            let input_handler: Arc<dyn UserInputHandler> = handler.clone();
            config = config
                .with_permission_handler(permission_handler)
                .with_elicitation_handler(elicitation_handler)
                .with_user_input_handler(input_handler);
        }
        config
    }

    fn configure_resume(&self, mut config: ResumeSessionConfig) -> ResumeSessionConfig {
        if let Some(handler) = &self.request_broker {
            let permission_handler: Arc<dyn PermissionHandler> = handler.clone();
            let elicitation_handler: Arc<dyn ElicitationHandler> = handler.clone();
            let input_handler: Arc<dyn UserInputHandler> = handler.clone();
            config = config
                .with_permission_handler(permission_handler)
                .with_elicitation_handler(elicitation_handler)
                .with_user_input_handler(input_handler);
        }
        config
    }
}

#[derive(Clone)]
pub struct StoreRequestBroker {
    store: Store,
}

impl StoreRequestBroker {
    pub fn new(store: Store) -> Self {
        Self { store }
    }

    fn create_request(
        &self,
        backend_session_id: &str,
        backend_request_id: Option<String>,
        kind: RequestKind,
        summary: String,
        mut payload: Value,
    ) -> Result<PendingRequest> {
        let session = self
            .store
            .get_session_by_backend_session_id(backend_session_id)?;
        if !payload.is_object() {
            payload = json!({ "raw": payload });
        }
        if let Some(backend_request_id) = backend_request_id {
            payload["backend_request_id"] = json!(backend_request_id);
        }
        payload["backend_session_id"] = json!(backend_session_id);
        payload["summary"] = json!(summary);
        let active_turn = self.store.active_turn_for_session(&session.session_id)?;
        let request = new_request(
            session.session_id.clone(),
            active_turn.as_ref().map(|turn| turn.turn_id.clone()),
            kind,
            backend_payload_summary(&payload),
            payload,
        );
        self.store.insert_request(&request)?;
        let parent_resource_uri = active_turn
            .as_ref()
            .map(|turn| turn.resource_uri.as_str())
            .unwrap_or(&session.resource_uri);
        self.store.append_event(
            &request.resource_uri,
            Some(parent_resource_uri),
            "request.created",
            "backend",
            COPILOT_BACKEND_ID,
            request.payload.clone(),
        )?;
        Ok(request)
    }

    async fn wait_for_resolution(&self, request_id: &str) -> Option<PendingRequest> {
        loop {
            match self.store.get_request(request_id) {
                Ok(request) if request.status != RequestStatus::Pending => return Some(request),
                Ok(_) => sleep(Duration::from_millis(250)).await,
                Err(_) => return None,
            }
        }
    }
}

#[async_trait]
impl PermissionHandler for StoreRequestBroker {
    async fn handle(
        &self,
        session_id: SdkSessionId,
        request_id: SdkRequestId,
        data: PermissionRequestData,
    ) -> PermissionResult {
        let mut payload =
            object_or_empty(serde_json::to_value(&data).unwrap_or_else(|_| json!({})));
        payload["request_kind"] = json!("permission");
        let request_kind = payload
            .get("kind")
            .and_then(Value::as_str)
            .unwrap_or("permission");
        let summary = format!("Copilot {request_kind} permission request");
        let request = match self.create_request(
            session_id.as_str(),
            Some(request_id.to_string()),
            RequestKind::Permission,
            summary,
            payload,
        ) {
            Ok(request) => request,
            Err(_) => return PermissionResult::user_not_available(),
        };
        let Some(resolved) = self.wait_for_resolution(&request.request_id).await else {
            return PermissionResult::user_not_available();
        };
        match resolved_decision(&resolved) {
            Some(RequestDecision::Approve) => PermissionResult::approve_once(),
            Some(RequestDecision::Respond) => {
                if response_value(&resolved)
                    .and_then(|response| response.get("approved").and_then(Value::as_bool))
                    .unwrap_or(false)
                {
                    PermissionResult::approve_once()
                } else {
                    PermissionResult::reject(resolved.reason)
                }
            }
            _ => PermissionResult::reject(resolved.reason),
        }
    }
}

#[async_trait]
impl UserInputHandler for StoreRequestBroker {
    async fn handle(
        &self,
        session_id: SdkSessionId,
        question: String,
        choices: Option<Vec<String>>,
        allow_freeform: Option<bool>,
    ) -> Option<UserInputResponse> {
        let payload = json!({
            "request_kind": "input",
            "backend_request_id": new_id("copilot_input"),
            "summary": question,
            "choices": choices,
            "allow_freeform": allow_freeform,
        });
        let request = self
            .create_request(
                session_id.as_str(),
                None,
                RequestKind::Input,
                question,
                payload,
            )
            .ok()?;
        let resolved = self.wait_for_resolution(&request.request_id).await?;
        match resolved_decision(&resolved) {
            Some(RequestDecision::Approve | RequestDecision::Respond) => {
                let response = response_value(&resolved)?;
                Some(UserInputResponse {
                    answer: response_answer(response),
                    was_freeform: true,
                })
            }
            _ => None,
        }
    }
}

#[async_trait]
impl ElicitationHandler for StoreRequestBroker {
    async fn handle(
        &self,
        session_id: SdkSessionId,
        request_id: SdkRequestId,
        request: ElicitationRequest,
    ) -> ElicitationResult {
        let mut payload =
            object_or_empty(serde_json::to_value(&request).unwrap_or_else(|_| json!({})));
        payload["request_kind"] = json!("elicitation");
        let summary = request.message;
        let request = match self.create_request(
            session_id.as_str(),
            Some(request_id.to_string()),
            RequestKind::Elicitation,
            summary,
            payload,
        ) {
            Ok(request) => request,
            Err(_) => {
                return ElicitationResult {
                    action: "cancel".to_string(),
                    content: None,
                };
            }
        };
        let Some(resolved) = self.wait_for_resolution(&request.request_id).await else {
            return ElicitationResult {
                action: "cancel".to_string(),
                content: None,
            };
        };
        match resolved_decision(&resolved) {
            Some(RequestDecision::Approve | RequestDecision::Respond) => ElicitationResult {
                action: "accept".to_string(),
                content: response_value(&resolved),
            },
            Some(RequestDecision::Deny) => ElicitationResult {
                action: "decline".to_string(),
                content: None,
            },
            _ => ElicitationResult {
                action: "cancel".to_string(),
                content: None,
            },
        }
    }
}

#[async_trait]
impl AgentBackend for CopilotBackend {
    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            backend_id: COPILOT_BACKEND_ID.to_string(),
            display_name: "GitHub Copilot".to_string(),
            supports_resume: true,
            supports_cancel: true,
            supports_permissions: true,
        }
    }

    async fn create_session(&self, config: BackendSessionConfig) -> Result<BackendSession> {
        let client = self.client().await?;
        let mut session_config = self.configure_session(SessionConfig::default());
        session_config.streaming = Some(true);
        session_config.model = config.model;
        if let Some(workspace) = config.workspace
            && let Some(path) = workspace.path
        {
            session_config.working_directory = Some(PathBuf::from(path));
        }
        let session = Arc::new(
            client
                .create_session(session_config)
                .await
                .map_err(copilot_err)?,
        );
        let backend_session_id = session.id().to_string();
        self.sessions
            .lock()
            .await
            .insert(backend_session_id.clone(), session);
        Ok(BackendSession {
            backend_id: COPILOT_BACKEND_ID.to_string(),
            backend_session_id,
            status: ResourceStatus::Idle,
        })
    }

    async fn resume_session(&self, id: BackendSessionId) -> Result<BackendSession> {
        let client = self.client().await?;
        let session = Arc::new(
            client
                .resume_session(
                    self.configure_resume(ResumeSessionConfig::new(SdkSessionId::new(id.clone()))),
                )
                .await
                .map_err(copilot_err)?,
        );
        self.sessions.lock().await.insert(id.clone(), session);
        Ok(BackendSession {
            backend_id: COPILOT_BACKEND_ID.to_string(),
            backend_session_id: id,
            status: ResourceStatus::Idle,
        })
    }

    async fn send_message(
        &self,
        session: &BackendSession,
        message: BackendMessage,
        event_sink: BackendEventSink,
    ) -> Result<BackendTurn> {
        let sdk_session = self.session(&session.backend_session_id).await?;
        let mut events = sdk_session.subscribe();
        let backend_turn_id = sdk_session
            .send(MessageOptions::new(message.content))
            .await
            .map_err(copilot_err)?;
        event_sink(BackendEvent {
            event_type: "turn.dispatched".to_string(),
            payload: json!({
                "backend": COPILOT_BACKEND_ID,
                "backend_turn_id": backend_turn_id.clone(),
                "singleton_turn_id": message.turn_id,
            }),
        })?;
        loop {
            let event = timeout(Duration::from_secs(60 * 60), events.recv())
                .await
                .map_err(|_| SingletonError::Backend {
                    backend: COPILOT_BACKEND_ID.to_string(),
                    message: "timed out waiting for Copilot session event".to_string(),
                })?;
            match event {
                Ok(event) => {
                    event_sink(normalize_sdk_event(&event))?;
                    if let Some(status) = terminal_status(&event) {
                        return Ok(BackendTurn {
                            backend_turn_id,
                            status,
                            events: vec![],
                        });
                    }
                }
                Err(error) => match error.kind() {
                    RecvErrorKind::Lagged(lagged) => event_sink(BackendEvent {
                        event_type: "subscription.lagged".to_string(),
                        payload: json!({ "skipped": lagged.skipped() }),
                    })?,
                    RecvErrorKind::Closed => {
                        return Err(SingletonError::Backend {
                            backend: COPILOT_BACKEND_ID.to_string(),
                            message: "Copilot event subscription closed before terminal event"
                                .to_string(),
                        });
                    }
                    _ => {
                        return Err(SingletonError::Backend {
                            backend: COPILOT_BACKEND_ID.to_string(),
                            message: format!("Copilot event subscription failed: {error}"),
                        });
                    }
                },
            }
        }
    }

    async fn cancel_turn(&self, session: &BackendSession, _turn_id: BackendTurnId) -> Result<()> {
        let sdk_session = self.session(&session.backend_session_id).await?;
        sdk_session.abort().await.map_err(copilot_err)
    }
}

fn normalize_sdk_event(event: &SessionEvent) -> BackendEvent {
    BackendEvent {
        event_type: event.event_type.clone(),
        payload: json!({
            "sdk_event_id": event.id.clone(),
            "sdk_timestamp": event.timestamp.clone(),
            "sdk_parent_id": event.parent_id.clone(),
            "sdk_agent_id": event.agent_id.clone(),
            "data": event.data.clone(),
        }),
    }
}

fn terminal_status(event: &SessionEvent) -> Option<ResourceStatus> {
    match event.event_type.as_str() {
        "assistant.turn_end" | "session.idle" => Some(ResourceStatus::Completed),
        "abort" => Some(ResourceStatus::Cancelled),
        "session.error" if !event.is_transient_error() => Some(ResourceStatus::Failed),
        _ => None,
    }
}

fn resolved_decision(request: &PendingRequest) -> Option<RequestDecision> {
    let value = request.resolution.as_ref()?.get("decision")?.clone();
    serde_json::from_value(value).ok()
}

fn response_value(request: &PendingRequest) -> Option<Value> {
    let response = request.resolution.as_ref()?.get("response")?.clone();
    if response.is_null() {
        None
    } else {
        Some(response)
    }
}

fn response_answer(response: Value) -> String {
    if let Some(answer) = response.get("answer").and_then(Value::as_str) {
        return answer.to_string();
    }
    if let Some(answer) = response.as_str() {
        return answer.to_string();
    }
    response.to_string()
}

fn object_or_empty(value: Value) -> Value {
    if value.is_object() {
        value
    } else {
        json!({ "raw": value })
    }
}

fn copilot_err(error: github_copilot_sdk::Error) -> SingletonError {
    SingletonError::Backend {
        backend: COPILOT_BACKEND_ID.to_string(),
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapter_reports_real_copilot_capabilities() {
        let backend = CopilotBackend::new(PathBuf::from("."));
        let caps = backend.capabilities();
        assert_eq!(caps.backend_id, COPILOT_BACKEND_ID);
        assert!(caps.supports_resume);
        assert!(caps.supports_cancel);
        assert!(caps.supports_permissions);
    }

    #[cfg(feature = "live-copilot")]
    #[tokio::test]
    #[ignore = "requires authenticated GitHub Copilot CLI access"]
    async fn live_copilot_create_and_send_session() -> Result<()> {
        let cwd = std::env::current_dir().map_err(|error| SingletonError::Backend {
            backend: COPILOT_BACKEND_ID.to_string(),
            message: format!("read current dir: {error}"),
        })?;
        let backend = CopilotBackend::new(cwd);
        let session = backend
            .create_session(BackendSessionConfig {
                description: "live smoke".to_string(),
                workspace: None,
                model: None,
                mode: None,
                labels: Vec::new(),
            })
            .await?;
        let turn = timeout(
            Duration::from_secs(120),
            backend.send_message(
                &session,
                BackendMessage {
                    turn_id: "turn_live".to_string(),
                    content: "Reply with exactly: singleton live smoke ok".to_string(),
                    mode: None,
                },
                Arc::new(|_| Ok(())),
            ),
        )
        .await
        .map_err(|_| SingletonError::Backend {
            backend: COPILOT_BACKEND_ID.to_string(),
            message: "timed out waiting for live Copilot smoke turn".to_string(),
        })??;
        assert!(matches!(
            turn.status,
            ResourceStatus::Completed | ResourceStatus::Cancelled
        ));
        Ok(())
    }
}
