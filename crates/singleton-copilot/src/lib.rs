use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use github_copilot_sdk::session::Session as SdkSession;
use github_copilot_sdk::types::{
    MessageOptions, ResumeSessionConfig, SessionConfig, SessionId as SdkSessionId,
};
use github_copilot_sdk::{Client, ClientOptions};
use serde_json::json;
use singleton_core::{
    AgentBackend, BackendCapabilities, BackendEvent, BackendMessage, BackendSession,
    BackendSessionConfig, BackendSessionId, BackendTurn, BackendTurnId, COPILOT_BACKEND_ID,
    ResourceStatus, Result, SingletonError,
};
use tokio::sync::Mutex;

#[derive(Clone)]
pub struct CopilotBackend {
    working_directory: PathBuf,
    client: Arc<Mutex<Option<Client>>>,
    sessions: Arc<Mutex<HashMap<BackendSessionId, Arc<SdkSession>>>>,
}

impl CopilotBackend {
    pub fn new(working_directory: PathBuf) -> Self {
        Self {
            working_directory,
            client: Arc::new(Mutex::new(None)),
            sessions: Arc::new(Mutex::new(HashMap::new())),
        }
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
        let mut session_config = SessionConfig::default();
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
                .resume_session(ResumeSessionConfig::new(SdkSessionId::new(id.clone())))
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
    ) -> Result<BackendTurn> {
        let sdk_session = self.session(&session.backend_session_id).await?;
        let backend_turn_id = sdk_session
            .send(MessageOptions::new(message.content))
            .await
            .map_err(copilot_err)?;
        Ok(BackendTurn {
            backend_turn_id,
            status: ResourceStatus::Running,
            events: vec![BackendEvent {
                event_type: "turn.started".to_string(),
                payload: json!({
                    "backend": COPILOT_BACKEND_ID,
                    "note": "Copilot turn dispatched; subscribe/read SDK events for completion ingestion"
                }),
            }],
        })
    }

    async fn cancel_turn(&self, session: &BackendSession, _turn_id: BackendTurnId) -> Result<()> {
        let sdk_session = self.session(&session.backend_session_id).await?;
        sdk_session.abort().await.map_err(copilot_err)
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
    async fn live_copilot_create_and_cancel_session() -> Result<()> {
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
        let turn = backend
            .send_message(
                &session,
                BackendMessage {
                    turn_id: "turn_live".to_string(),
                    content: "Say hello and then stop.".to_string(),
                    mode: None,
                },
            )
            .await?;
        backend.cancel_turn(&session, turn.backend_turn_id).await?;
        Ok(())
    }
}
