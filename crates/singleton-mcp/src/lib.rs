use rmcp::{
    Json, ServerHandler,
    handler::server::{router::tool::ToolRouter, wrapper::Parameters},
    tool, tool_handler, tool_router,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use singleton_broker::{
    Broker, CancelTurnReply, CancelTurnRequest, CloseResourceReply, CloseResourceRequest,
    CreateSessionReply, CreateSessionRequest, ReadEventsReply, ReadEventsRequest, ResolveRequest,
    SendMessageReply, SendMessageRequest, SessionDetail,
};
use singleton_core::{
    AgentBackend, Capabilities, HostConnector, Inbox, PendingRequest, Result as SingletonResult,
    Session, SingletonError, Workspace, WorkspaceSpec,
};

#[tool_handler(router = self.tool_router)]
impl<B, H> ServerHandler for SingletonMcpServer<B, H>
where
    B: AgentBackend + 'static,
    H: HostConnector + 'static,
{
}

#[derive(Clone)]
pub struct SingletonMcpServer<B, H>
where
    B: AgentBackend + 'static,
    H: HostConnector + 'static,
{
    broker: Broker<B, H>,
    tool_router: ToolRouter<Self>,
}

impl<B, H> SingletonMcpServer<B, H>
where
    B: AgentBackend + 'static,
    H: HostConnector + 'static,
{
    pub fn new(broker: Broker<B, H>) -> Self {
        Self {
            broker,
            tool_router: Self::tool_router(),
        }
    }

    pub fn tool_names(&self) -> Vec<String> {
        self.tool_router
            .list_all()
            .iter()
            .map(|tool| tool.name.to_string())
            .collect()
    }
}

#[tool_router(router = tool_router)]
impl<B, H> SingletonMcpServer<B, H>
where
    B: AgentBackend + 'static,
    H: HostConnector + 'static,
{
    #[tool(
        description = "Return singleton protocol, host, backend, and default tool capabilities."
    )]
    pub async fn get_capabilities(&self) -> std::result::Result<Json<Capabilities>, String> {
        Ok(Json(self.broker.get_capabilities()))
    }

    #[tool(
        description = "Return compact fan-in state for pending requests, failed turns, completions, and stale sessions."
    )]
    pub async fn get_inbox(&self) -> std::result::Result<Json<Inbox>, String> {
        mcp_json(self.broker.get_inbox())
    }

    #[tool(description = "Create or resolve a workspace, including local paths and git worktrees.")]
    pub async fn ensure_workspace(
        &self,
        Parameters(request): Parameters<EnsureWorkspaceRequest>,
    ) -> std::result::Result<Json<Workspace>, String> {
        mcp_json(self.broker.ensure_workspace(request.spec).await)
    }

    #[tool(description = "Create a durable background agent session.")]
    pub async fn create_session(
        &self,
        Parameters(request): Parameters<CreateSessionRequest>,
    ) -> std::result::Result<Json<CreateSessionReply>, String> {
        mcp_json(self.broker.create_session(request).await)
    }

    #[tool(description = "Start an asynchronous turn in a session.")]
    pub async fn send_message(
        &self,
        Parameters(request): Parameters<SendMessageRequest>,
    ) -> std::result::Result<Json<SendMessageReply>, String> {
        mcp_json(self.broker.send_message(request).await)
    }

    #[tool(description = "Read or long-poll ordered events for a session or resource.")]
    pub async fn read_events(
        &self,
        Parameters(request): Parameters<ReadEventsRequest>,
    ) -> std::result::Result<Json<ReadEventsReply>, String> {
        mcp_json(self.broker.read_events(request).await)
    }

    #[tool(description = "List active and recent background sessions.")]
    pub async fn list_sessions(&self) -> std::result::Result<Json<ListSessionsReply>, String> {
        mcp_json(
            self.broker
                .list_sessions()
                .map(|sessions| ListSessionsReply { sessions }),
        )
    }

    #[tool(description = "Inspect one session, including workspace and pending request summary.")]
    pub async fn get_session(
        &self,
        Parameters(request): Parameters<GetSessionRequest>,
    ) -> std::result::Result<Json<SessionDetail>, String> {
        mcp_json(self.broker.get_session(&request.session_id))
    }

    #[tool(description = "Resolve a pending permission, input, or elicitation request.")]
    pub async fn resolve_request(
        &self,
        Parameters(request): Parameters<ResolveRequest>,
    ) -> std::result::Result<Json<PendingRequest>, String> {
        mcp_json(self.broker.resolve_request(request))
    }

    #[tool(description = "Cancel a running turn.")]
    pub async fn cancel_turn(
        &self,
        Parameters(request): Parameters<CancelTurnRequest>,
    ) -> std::result::Result<Json<CancelTurnReply>, String> {
        mcp_json(self.broker.cancel_turn(request).await)
    }

    #[tool(
        description = "Archive, dispose, or delete sessions and workspaces with safe cleanup rules."
    )]
    pub async fn close_resource(
        &self,
        Parameters(request): Parameters<CloseResourceRequest>,
    ) -> std::result::Result<Json<CloseResourceReply>, String> {
        mcp_json(self.broker.close_resource(request).await)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct EnsureWorkspaceRequest {
    pub spec: WorkspaceSpec,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct GetSessionRequest {
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct ListSessionsReply {
    pub sessions: Vec<Session>,
}

fn mcp_json<T>(result: SingletonResult<T>) -> std::result::Result<Json<T>, String> {
    result.map(Json).map_err(mcp_error)
}

fn mcp_error(error: SingletonError) -> String {
    error.to_string()
}

#[cfg(test)]
mod tests {
    use singleton_broker::CreateSessionRequest;
    use singleton_core::{DEFAULT_MCP_TOOLS, ResourceStatus, WorkspaceSpec};
    use singleton_host::LocalHostConnector;
    use singleton_store::Store;
    use singleton_test_support::FakeBackend;
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn default_tool_profile_matches_spec() -> SingletonResult<()> {
        let server = SingletonMcpServer::new(Broker::new(
            Store::open_memory()?,
            FakeBackend::new(),
            LocalHostConnector,
        ));
        let names = server.tool_names();
        for expected in DEFAULT_MCP_TOOLS {
            assert!(
                names.contains(&expected.to_string()),
                "missing MCP tool {expected}"
            );
        }
        Ok(())
    }

    #[tokio::test]
    async fn typed_mcp_facade_runs_vertical_slice() -> SingletonResult<()> {
        let temp = TempDir::new()
            .map_err(|error| SingletonError::Store(format!("create temp dir: {error}")))?;
        let server = SingletonMcpServer::new(Broker::new(
            Store::open_memory()?,
            FakeBackend::new(),
            LocalHostConnector,
        ));
        let Json(created) = server
            .create_session(Parameters(CreateSessionRequest {
                description: "MCP vertical slice".to_string(),
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
            }))
            .await
            .map_err(SingletonError::InvalidInput)?;
        let Json(sent) = server
            .send_message(Parameters(SendMessageRequest {
                session_id: created.session_id.clone(),
                message: "hello".to_string(),
                mode: None,
            }))
            .await
            .map_err(SingletonError::InvalidInput)?;

        assert_eq!(sent.status, ResourceStatus::Running);
        let Json(events) = server
            .read_events(Parameters(ReadEventsRequest {
                session_id: Some(created.session_id),
                resource_uri: None,
                cursor: Some(0),
                limit: Some(100),
                event_types: vec!["turn.completed".to_string()],
                wait_ms: Some(1_000),
            }))
            .await
            .map_err(SingletonError::InvalidInput)?;
        assert!(
            events
                .events
                .iter()
                .any(|event| event.event_type == "turn.completed")
        );
        Ok(())
    }
}
