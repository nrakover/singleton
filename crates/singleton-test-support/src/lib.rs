use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::json;
use singleton_core::{
    AgentBackend, BackendCapabilities, BackendEvent, BackendEventSink, BackendMessage,
    BackendSession, BackendSessionConfig, BackendSessionId, BackendTurn, BackendTurnId,
    FAKE_BACKEND_ID, ResourceStatus, Result, SingletonError, new_id,
};

#[derive(Debug, Clone)]
pub enum FakeTurnBehavior {
    Complete { summary: String },
    CompleteWithoutOutput,
    Fail { summary: String },
    RequestPermission { summary: String },
    RequestInput { prompt: String },
    StayRunning,
}

impl Default for FakeTurnBehavior {
    fn default() -> Self {
        Self::Complete {
            summary: "fake turn completed".to_string(),
        }
    }
}

#[derive(Clone, Default)]
pub struct FakeBackend {
    state: Arc<Mutex<FakeBackendState>>,
}

#[derive(Default)]
struct FakeBackendState {
    queued_behaviors: VecDeque<FakeTurnBehavior>,
    cancelled_turns: Vec<BackendTurnId>,
}

impl FakeBackend {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_behaviors(behaviors: impl IntoIterator<Item = FakeTurnBehavior>) -> Self {
        Self {
            state: Arc::new(Mutex::new(FakeBackendState {
                queued_behaviors: behaviors.into_iter().collect(),
                cancelled_turns: Vec::new(),
            })),
        }
    }

    pub fn cancelled_turns(&self) -> Result<Vec<BackendTurnId>> {
        Ok(self
            .state
            .lock()
            .map_err(|_| fake_lock_err())?
            .cancelled_turns
            .clone())
    }
}

#[async_trait]
impl AgentBackend for FakeBackend {
    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities {
            backend_id: FAKE_BACKEND_ID.to_string(),
            display_name: "Fake backend".to_string(),
            supports_resume: true,
            supports_turn_reattach: true,
            supports_cancel: true,
            supports_permissions: true,
        }
    }

    async fn create_session(&self, _config: BackendSessionConfig) -> Result<BackendSession> {
        Ok(BackendSession {
            backend_id: FAKE_BACKEND_ID.to_string(),
            backend_session_id: new_id("fake_sess"),
            status: ResourceStatus::Idle,
        })
    }

    async fn resume_session(&self, id: BackendSessionId) -> Result<BackendSession> {
        Ok(BackendSession {
            backend_id: FAKE_BACKEND_ID.to_string(),
            backend_session_id: id,
            status: ResourceStatus::Idle,
        })
    }

    async fn send_message(
        &self,
        _session: &BackendSession,
        message: BackendMessage,
        event_sink: BackendEventSink,
    ) -> Result<BackendTurn> {
        let behavior = self
            .state
            .lock()
            .map_err(|_| fake_lock_err())?
            .queued_behaviors
            .pop_front()
            .unwrap_or_default();
        let backend_turn_id = new_id("fake_turn");
        let turn = match behavior {
            FakeTurnBehavior::Complete { summary } => {
                event_sink(BackendEvent {
                    event_type: "message.delta".to_string(),
                    payload: json!({ "content": format!("processed: {}", message.content) }),
                })?;
                BackendTurn {
                    backend_turn_id,
                    status: ResourceStatus::Completed,
                    events: vec![BackendEvent {
                        event_type: "turn.completed".to_string(),
                        payload: json!({ "summary": summary }),
                    }],
                }
            }
            FakeTurnBehavior::CompleteWithoutOutput => BackendTurn {
                backend_turn_id,
                status: ResourceStatus::Completed,
                events: Vec::new(),
            },
            FakeTurnBehavior::Fail { summary } => BackendTurn {
                backend_turn_id,
                status: ResourceStatus::Failed,
                events: vec![BackendEvent {
                    event_type: "turn.failed".to_string(),
                    payload: json!({ "summary": summary, "retryable": true }),
                }],
            },
            FakeTurnBehavior::RequestPermission { summary } => BackendTurn {
                backend_turn_id,
                status: ResourceStatus::NeedsInput,
                events: vec![BackendEvent {
                    event_type: "request.created".to_string(),
                    payload: json!({
                        "request_kind": "permission",
                        "summary": summary,
                        "tool": "bash"
                    }),
                }],
            },
            FakeTurnBehavior::RequestInput { prompt } => BackendTurn {
                backend_turn_id,
                status: ResourceStatus::NeedsInput,
                events: vec![BackendEvent {
                    event_type: "request.created".to_string(),
                    payload: json!({
                        "request_kind": "input",
                        "summary": prompt,
                        "choices": ["yes", "no"]
                    }),
                }],
            },
            FakeTurnBehavior::StayRunning => {
                event_sink(BackendEvent {
                    event_type: "turn.started".to_string(),
                    payload: json!({ "summary": "fake turn still running" }),
                })?;
                BackendTurn {
                    backend_turn_id,
                    status: ResourceStatus::Running,
                    events: vec![],
                }
            }
        };
        Ok(turn)
    }

    async fn cancel_turn(&self, _session: &BackendSession, turn_id: BackendTurnId) -> Result<()> {
        self.state
            .lock()
            .map_err(|_| fake_lock_err())?
            .cancelled_turns
            .push(turn_id);
        Ok(())
    }

    async fn reattach_turn(
        &self,
        _session: &BackendSession,
        turn: &singleton_core::Turn,
        event_sink: BackendEventSink,
    ) -> Result<Option<BackendTurn>> {
        let backend_turn_id = turn
            .backend_turn_id
            .clone()
            .unwrap_or_else(|| new_id("fake_turn"));
        event_sink(BackendEvent {
            event_type: "turn.reattached".to_string(),
            payload: json!({
                "backend_turn_id": backend_turn_id.clone(),
                "summary": "fake active turn reattached"
            }),
        })?;
        Ok(Some(BackendTurn {
            backend_turn_id,
            status: ResourceStatus::Completed,
            events: vec![BackendEvent {
                event_type: "turn.completed".to_string(),
                payload: json!({ "summary": "fake reattached turn completed" }),
            }],
        }))
    }
}

fn fake_lock_err() -> SingletonError {
    SingletonError::Backend {
        backend: FAKE_BACKEND_ID.to_string(),
        message: "fake backend lock poisoned".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use singleton_core::{BackendMessage, BackendSessionConfig};

    use super::*;

    #[tokio::test]
    async fn fake_backend_emits_deterministic_completion() -> Result<()> {
        let backend = FakeBackend::new();
        let session = backend
            .create_session(BackendSessionConfig {
                description: "test".to_string(),
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
                    turn_id: "turn_test".to_string(),
                    content: "hello".to_string(),
                    mode: None,
                },
                Arc::new(|_| Ok(())),
            )
            .await?;

        assert_eq!(turn.status, ResourceStatus::Completed);
        assert_eq!(turn.events.len(), 1);
        Ok(())
    }
}
