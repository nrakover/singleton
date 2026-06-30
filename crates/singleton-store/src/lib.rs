use std::path::Path;
use std::sync::{Arc, Mutex};

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use singleton_core::{
    BackendSessionId, CloseDisposition, Event, ForwardedOperation, ForwardedOperationStatus,
    PendingRequest, RemoteBrokerIdentity, RemoteHostHealth, RemoteResourceLink, RepoMetadata,
    RequestDecision, RequestKind, RequestStatus, ResourceKind, ResourceStatus, Result, Session,
    SessionId, SingletonError, Turn, TurnId, Workspace, WorkspaceId, new_id, now_rfc3339,
    resource_uri,
};

#[derive(Clone)]
pub struct Store {
    conn: Arc<Mutex<Connection>>,
}

impl Store {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let conn = Connection::open(path).map_err(store_err)?;
        let store = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        store.migrate()?;
        Ok(store)
    }

    pub fn open_memory() -> Result<Self> {
        let conn = Connection::open_in_memory().map_err(store_err)?;
        let store = Self {
            conn: Arc::new(Mutex::new(conn)),
        };
        store.migrate()?;
        Ok(store)
    }

    pub fn migrate(&self) -> Result<()> {
        let conn = self.conn.lock().map_err(|_| lock_err())?;
        conn.execute_batch(
            r#"
            PRAGMA foreign_keys = ON;

            CREATE TABLE IF NOT EXISTS hosts (
                host_id TEXT PRIMARY KEY,
                resource_uri TEXT NOT NULL UNIQUE,
                kind TEXT NOT NULL,
                status TEXT NOT NULL,
                capabilities_json TEXT NOT NULL,
                created_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS workspaces (
                workspace_id TEXT PRIMARY KEY,
                resource_uri TEXT NOT NULL UNIQUE,
                host_id TEXT NOT NULL,
                status TEXT NOT NULL,
                path TEXT,
                repo_json TEXT,
                cleanup_policy TEXT NOT NULL,
                created_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS sessions (
                session_id TEXT PRIMARY KEY,
                resource_uri TEXT NOT NULL UNIQUE,
                title TEXT NOT NULL,
                description TEXT,
                backend TEXT NOT NULL,
                backend_session_id TEXT,
                workspace_id TEXT,
                status TEXT NOT NULL,
                latest_event_cursor INTEGER NOT NULL DEFAULT 0,
                labels_json TEXT NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS chats (
                chat_id TEXT PRIMARY KEY,
                resource_uri TEXT NOT NULL UNIQUE,
                session_id TEXT NOT NULL,
                status TEXT NOT NULL,
                created_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS turns (
                turn_id TEXT PRIMARY KEY,
                resource_uri TEXT NOT NULL UNIQUE,
                session_id TEXT NOT NULL,
                backend_turn_id TEXT,
                message TEXT NOT NULL,
                status TEXT NOT NULL,
                unread INTEGER NOT NULL,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS requests (
                request_id TEXT PRIMARY KEY,
                resource_uri TEXT NOT NULL UNIQUE,
                session_id TEXT NOT NULL,
                turn_id TEXT,
                kind TEXT NOT NULL,
                status TEXT NOT NULL,
                summary TEXT NOT NULL,
                payload_json TEXT NOT NULL,
                resolution_json TEXT,
                reason TEXT,
                created_at TEXT NOT NULL,
                resolved_at TEXT
            );

            CREATE TABLE IF NOT EXISTS events (
                server_seq INTEGER PRIMARY KEY AUTOINCREMENT,
                event_id TEXT NOT NULL UNIQUE,
                resource_uri TEXT NOT NULL,
                parent_resource_uri TEXT,
                event_type TEXT NOT NULL,
                origin_kind TEXT NOT NULL,
                origin_id TEXT NOT NULL,
                payload_json TEXT NOT NULL,
                created_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS resource_states (
                resource_uri TEXT PRIMARY KEY,
                resource_kind TEXT NOT NULL,
                state_json TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS changesets (
                changeset_id TEXT PRIMARY KEY,
                resource_uri TEXT NOT NULL UNIQUE,
                workspace_id TEXT NOT NULL,
                summary_json TEXT NOT NULL,
                created_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS artifacts (
                artifact_id TEXT PRIMARY KEY,
                resource_uri TEXT NOT NULL UNIQUE,
                owner_resource_uri TEXT NOT NULL,
                path TEXT NOT NULL,
                media_type TEXT,
                created_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS remote_hosts (
                host_id TEXT PRIMARY KEY,
                state TEXT NOT NULL,
                remote_identity_json TEXT,
                capabilities_json TEXT,
                last_checked_at TEXT,
                last_success_at TEXT,
                last_error TEXT,
                updated_at TEXT NOT NULL
            );

            CREATE TABLE IF NOT EXISTS remote_resource_links (
                local_resource_uri TEXT PRIMARY KEY,
                local_resource_kind TEXT NOT NULL,
                local_id TEXT NOT NULL,
                host_id TEXT NOT NULL,
                remote_resource_uri TEXT NOT NULL,
                remote_id TEXT NOT NULL,
                remote_cursor INTEGER NOT NULL DEFAULT 0,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                UNIQUE(host_id, remote_resource_uri)
            );

            CREATE TABLE IF NOT EXISTS forwarded_operations (
                operation_id TEXT PRIMARY KEY,
                host_id TEXT NOT NULL,
                operation_kind TEXT NOT NULL,
                status TEXT NOT NULL,
                local_resource_uri TEXT,
                request_json TEXT NOT NULL,
                result_json TEXT,
                error TEXT,
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_events_resource_seq ON events(resource_uri, server_seq);
            CREATE INDEX IF NOT EXISTS idx_events_parent_seq ON events(parent_resource_uri, server_seq);
            CREATE INDEX IF NOT EXISTS idx_requests_status ON requests(status);
            CREATE INDEX IF NOT EXISTS idx_turns_session_status ON turns(session_id, status);
            CREATE INDEX IF NOT EXISTS idx_remote_links_host ON remote_resource_links(host_id);
            CREATE INDEX IF NOT EXISTS idx_forwarded_operations_host_status ON forwarded_operations(host_id, status);
            "#,
        )
        .map_err(store_err)?;
        Ok(())
    }

    pub fn insert_workspace(&self, workspace: &Workspace) -> Result<()> {
        let conn = self.conn.lock().map_err(|_| lock_err())?;
        conn.execute(
            r#"
            INSERT OR REPLACE INTO workspaces
            (workspace_id, resource_uri, host_id, status, path, repo_json, cleanup_policy, created_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            "#,
            params![
                workspace.workspace_id,
                workspace.resource_uri,
                workspace.host_id,
                status_to_string(&workspace.status)?,
                workspace.path,
                opt_json(&workspace.repo)?,
                enum_to_string(&workspace.cleanup_policy)?,
                workspace.created_at
            ],
        )
        .map_err(store_err)?;
        drop(conn);
        self.put_resource_state(&workspace.resource_uri, ResourceKind::Workspace, workspace)?;
        Ok(())
    }

    pub fn get_workspace(&self, workspace_id: &str) -> Result<Workspace> {
        let conn = self.conn.lock().map_err(|_| lock_err())?;
        let row = conn
            .query_row(
                r#"
                SELECT workspace_id, resource_uri, host_id, status, path, repo_json, cleanup_policy, created_at
                FROM workspaces
                WHERE workspace_id = ?1
                "#,
                params![workspace_id],
                |row| {
                    Ok(WorkspaceRow {
                        workspace_id: row.get(0)?,
                        resource_uri: row.get(1)?,
                        host_id: row.get(2)?,
                        status: row.get(3)?,
                        path: row.get(4)?,
                        repo_json: row.get(5)?,
                        cleanup_policy: row.get(6)?,
                        created_at: row.get(7)?,
                    })
                },
            )
            .optional()
            .map_err(store_err)?
            .ok_or_else(|| SingletonError::NotFound {
                resource: "workspace",
                id: workspace_id.to_string(),
            })?;
        row.try_into_workspace()
    }

    pub fn update_workspace_status(
        &self,
        workspace_id: &str,
        status: ResourceStatus,
    ) -> Result<()> {
        let conn = self.conn.lock().map_err(|_| lock_err())?;
        conn.execute(
            "UPDATE workspaces SET status = ?2 WHERE workspace_id = ?1",
            params![workspace_id, status_to_string(&status)?],
        )
        .map_err(store_err)?;
        Ok(())
    }

    pub fn insert_session(&self, session: &Session) -> Result<()> {
        let conn = self.conn.lock().map_err(|_| lock_err())?;
        conn.execute(
            r#"
            INSERT OR REPLACE INTO sessions
            (session_id, resource_uri, title, description, backend, backend_session_id, workspace_id,
             status, latest_event_cursor, labels_json, created_at, updated_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
            "#,
            params![
                session.session_id,
                session.resource_uri,
                session.title,
                session.description,
                session.backend,
                session.backend_session_id,
                session.workspace_id,
                status_to_string(&session.status)?,
                session.latest_event_cursor,
                json_string(&session.labels)?,
                session.created_at,
                session.updated_at
            ],
        )
        .map_err(store_err)?;
        drop(conn);
        self.put_resource_state(&session.resource_uri, ResourceKind::Session, session)?;
        Ok(())
    }

    pub fn get_session(&self, session_id: &str) -> Result<Session> {
        let conn = self.conn.lock().map_err(|_| lock_err())?;
        let row = conn
            .query_row(
                r#"
                SELECT session_id, resource_uri, title, description, backend, backend_session_id,
                       workspace_id, status, latest_event_cursor, labels_json, created_at, updated_at
                FROM sessions
                WHERE session_id = ?1
                "#,
                params![session_id],
                session_row,
            )
            .optional()
            .map_err(store_err)?
            .ok_or_else(|| SingletonError::NotFound {
                resource: "session",
                id: session_id.to_string(),
            })?;
        row.try_into_session()
    }

    pub fn get_session_by_backend_session_id(&self, backend_session_id: &str) -> Result<Session> {
        let conn = self.conn.lock().map_err(|_| lock_err())?;
        let row = conn
            .query_row(
                r#"
                SELECT session_id, resource_uri, title, description, backend, backend_session_id,
                       workspace_id, status, latest_event_cursor, labels_json, created_at, updated_at
                FROM sessions
                WHERE backend_session_id = ?1
                "#,
                params![backend_session_id],
                session_row,
            )
            .optional()
            .map_err(store_err)?
            .ok_or_else(|| SingletonError::NotFound {
                resource: "backend_session",
                id: backend_session_id.to_string(),
            })?;
        row.try_into_session()
    }

    pub fn list_sessions(&self) -> Result<Vec<Session>> {
        let conn = self.conn.lock().map_err(|_| lock_err())?;
        let mut stmt = conn
            .prepare(
                r#"
                SELECT session_id, resource_uri, title, description, backend, backend_session_id,
                       workspace_id, status, latest_event_cursor, labels_json, created_at, updated_at
                FROM sessions
                ORDER BY updated_at DESC
                "#,
            )
            .map_err(store_err)?;
        let mut rows = stmt.query([]).map_err(store_err)?;
        let mut sessions = Vec::new();
        while let Some(row) = rows.next().map_err(store_err)? {
            sessions.push(session_row(row).map_err(store_err)?.try_into_session()?);
        }
        Ok(sessions)
    }

    pub fn sessions_with_backend_session_ids(&self) -> Result<Vec<Session>> {
        let conn = self.conn.lock().map_err(|_| lock_err())?;
        let mut stmt = conn
            .prepare(
                r#"
                SELECT session_id, resource_uri, title, description, backend, backend_session_id,
                       workspace_id, status, latest_event_cursor, labels_json, created_at, updated_at
                FROM sessions
                WHERE backend_session_id IS NOT NULL
                  AND status NOT IN ('archived', 'disposed', 'deleted')
                ORDER BY updated_at ASC
                "#,
            )
            .map_err(store_err)?;
        let mut rows = stmt.query([]).map_err(store_err)?;
        let mut sessions = Vec::new();
        while let Some(row) = rows.next().map_err(store_err)? {
            sessions.push(session_row(row).map_err(store_err)?.try_into_session()?);
        }
        Ok(sessions)
    }

    pub fn update_session_backend(
        &self,
        session_id: &str,
        backend_session_id: &BackendSessionId,
        status: ResourceStatus,
    ) -> Result<()> {
        let now = now_rfc3339();
        let conn = self.conn.lock().map_err(|_| lock_err())?;
        conn.execute(
            r#"
            UPDATE sessions
            SET backend_session_id = ?2, status = ?3, updated_at = ?4
            WHERE session_id = ?1
            "#,
            params![
                session_id,
                backend_session_id,
                status_to_string(&status)?,
                now
            ],
        )
        .map_err(store_err)?;
        Ok(())
    }

    pub fn update_session_status(
        &self,
        session_id: &str,
        status: ResourceStatus,
        latest_event_cursor: Option<i64>,
    ) -> Result<()> {
        let now = now_rfc3339();
        let conn = self.conn.lock().map_err(|_| lock_err())?;
        conn.execute(
            r#"
            UPDATE sessions
            SET status = ?2,
                latest_event_cursor = COALESCE(?3, latest_event_cursor),
                updated_at = ?4
            WHERE session_id = ?1
            "#,
            params![
                session_id,
                status_to_string(&status)?,
                latest_event_cursor,
                now
            ],
        )
        .map_err(store_err)?;
        Ok(())
    }

    pub fn insert_turn(&self, turn: &Turn) -> Result<()> {
        let conn = self.conn.lock().map_err(|_| lock_err())?;
        conn.execute(
            r#"
            INSERT OR REPLACE INTO turns
            (turn_id, resource_uri, session_id, backend_turn_id, message, status, unread, created_at, updated_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
            "#,
            params![
                turn.turn_id,
                turn.resource_uri,
                turn.session_id,
                turn.backend_turn_id,
                turn.message,
                status_to_string(&turn.status)?,
                bool_to_i64(turn.unread),
                turn.created_at,
                turn.updated_at
            ],
        )
        .map_err(store_err)?;
        drop(conn);
        self.put_resource_state(&turn.resource_uri, ResourceKind::Turn, turn)?;
        Ok(())
    }

    pub fn get_turn(&self, turn_id: &str) -> Result<Turn> {
        let conn = self.conn.lock().map_err(|_| lock_err())?;
        let row = conn
            .query_row(
                r#"
                SELECT turn_id, resource_uri, session_id, backend_turn_id, message, status, unread, created_at, updated_at
                FROM turns
                WHERE turn_id = ?1
                "#,
                params![turn_id],
                turn_row,
            )
            .optional()
            .map_err(store_err)?
            .ok_or_else(|| SingletonError::NotFound {
                resource: "turn",
                id: turn_id.to_string(),
            })?;
        row.try_into_turn()
    }

    pub fn update_turn_status(
        &self,
        turn_id: &str,
        backend_turn_id: Option<&str>,
        status: ResourceStatus,
        unread: bool,
    ) -> Result<()> {
        let now = now_rfc3339();
        let conn = self.conn.lock().map_err(|_| lock_err())?;
        conn.execute(
            r#"
            UPDATE turns
            SET backend_turn_id = COALESCE(?2, backend_turn_id),
                status = ?3,
                unread = ?4,
                updated_at = ?5
            WHERE turn_id = ?1
            "#,
            params![
                turn_id,
                backend_turn_id,
                status_to_string(&status)?,
                bool_to_i64(unread),
                now
            ],
        )
        .map_err(store_err)?;
        Ok(())
    }

    pub fn active_turn_for_session(&self, session_id: &str) -> Result<Option<Turn>> {
        let conn = self.conn.lock().map_err(|_| lock_err())?;
        let mut stmt = conn
            .prepare(
                r#"
                SELECT turn_id, resource_uri, session_id, backend_turn_id, message, status, unread, created_at, updated_at
                FROM turns
                WHERE session_id = ?1 AND status IN ('queued', 'running', 'needs_input')
                ORDER BY created_at DESC
                LIMIT 1
                "#,
            )
            .map_err(store_err)?;
        let mut rows = stmt.query(params![session_id]).map_err(store_err)?;
        if let Some(row) = rows.next().map_err(store_err)? {
            Ok(Some(turn_row(row).map_err(store_err)?.try_into_turn()?))
        } else {
            Ok(None)
        }
    }

    pub fn latest_terminal_turn_for_session(&self, session_id: &str) -> Result<Option<Turn>> {
        let conn = self.conn.lock().map_err(|_| lock_err())?;
        let mut stmt = conn
            .prepare(
                r#"
                SELECT turn_id, resource_uri, session_id, backend_turn_id, message, status, unread, created_at, updated_at
                FROM turns
                WHERE session_id = ?1 AND status IN ('completed', 'failed', 'cancelled')
                ORDER BY updated_at DESC, created_at DESC
                LIMIT 1
                "#,
            )
            .map_err(store_err)?;
        let mut rows = stmt.query(params![session_id]).map_err(store_err)?;
        if let Some(row) = rows.next().map_err(store_err)? {
            Ok(Some(turn_row(row).map_err(store_err)?.try_into_turn()?))
        } else {
            Ok(None)
        }
    }

    pub fn inbox_turns(&self) -> Result<Vec<Turn>> {
        let conn = self.conn.lock().map_err(|_| lock_err())?;
        let mut stmt = conn
            .prepare(
                r#"
                SELECT turn_id, resource_uri, session_id, backend_turn_id, message, status, unread, created_at, updated_at
                FROM turns
                WHERE status IN ('completed', 'failed') AND unread = 1
                ORDER BY updated_at DESC
                "#,
            )
            .map_err(store_err)?;
        let mut rows = stmt.query([]).map_err(store_err)?;
        let mut turns = Vec::new();
        while let Some(row) = rows.next().map_err(store_err)? {
            turns.push(turn_row(row).map_err(store_err)?.try_into_turn()?);
        }
        Ok(turns)
    }

    pub fn acknowledge_inbox_turns(
        &self,
        session_id: Option<&str>,
        turn_id: Option<&str>,
        all: bool,
    ) -> Result<usize> {
        let now = now_rfc3339();
        let conn = self.conn.lock().map_err(|_| lock_err())?;
        match (turn_id, session_id, all) {
            (Some(turn_id), _, _) => {
                conn.execute(
                    r#"
                    UPDATE turns
                    SET unread = 0, updated_at = ?2
                    WHERE turn_id = ?1 AND unread = 1
                    "#,
                    params![turn_id, now],
                )
                .map_err(store_err)?;
            }
            (None, Some(session_id), _) => {
                conn.execute(
                    r#"
                    UPDATE turns
                    SET unread = 0, updated_at = ?2
                    WHERE session_id = ?1 AND unread = 1
                    "#,
                    params![session_id, now],
                )
                .map_err(store_err)?;
            }
            (None, None, true) => {
                conn.execute(
                    r#"
                    UPDATE turns
                    SET unread = 0, updated_at = ?1
                    WHERE unread = 1
                    "#,
                    params![now],
                )
                .map_err(store_err)?;
            }
            (None, None, false) => {
                return Err(SingletonError::InvalidInput(
                    "acknowledge requires turn_id, session_id, or all=true".to_string(),
                ));
            }
        }
        usize::try_from(conn.changes()).map_err(|error| {
            SingletonError::Store(format!("invalid acknowledged count conversion: {error}"))
        })
    }

    pub fn insert_request(&self, request: &PendingRequest) -> Result<()> {
        let conn = self.conn.lock().map_err(|_| lock_err())?;
        conn.execute(
            r#"
            INSERT OR REPLACE INTO requests
            (request_id, resource_uri, session_id, turn_id, kind, status, summary, payload_json,
             resolution_json, reason, created_at, resolved_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
            "#,
            params![
                request.request_id,
                request.resource_uri,
                request.session_id,
                request.turn_id,
                enum_to_string(&request.kind)?,
                enum_to_string(&request.status)?,
                request.summary,
                json_string(&request.payload)?,
                opt_json(&request.resolution)?,
                request.reason,
                request.created_at,
                request.resolved_at
            ],
        )
        .map_err(store_err)?;
        drop(conn);
        self.put_resource_state(&request.resource_uri, ResourceKind::Request, request)?;
        Ok(())
    }

    pub fn pending_requests(&self) -> Result<Vec<PendingRequest>> {
        let conn = self.conn.lock().map_err(|_| lock_err())?;
        let mut stmt = conn
            .prepare(
                r#"
                SELECT request_id, resource_uri, session_id, turn_id, kind, status, summary,
                       payload_json, resolution_json, reason, created_at, resolved_at
                FROM requests
                WHERE status = 'pending'
                ORDER BY created_at ASC
                "#,
            )
            .map_err(store_err)?;
        let mut rows = stmt.query([]).map_err(store_err)?;
        let mut requests = Vec::new();
        while let Some(row) = rows.next().map_err(store_err)? {
            requests.push(request_row(row).map_err(store_err)?.try_into_request()?);
        }
        Ok(requests)
    }

    pub fn resolve_request(
        &self,
        request_id: &str,
        decision: RequestDecision,
        response: Option<Value>,
        reason: Option<String>,
    ) -> Result<PendingRequest> {
        let status = match decision {
            RequestDecision::Cancel => RequestStatus::Cancelled,
            RequestDecision::Approve | RequestDecision::Deny | RequestDecision::Respond => {
                RequestStatus::Resolved
            }
        };
        let resolution = json!({
            "decision": decision,
            "response": response,
        });
        let resolved_at = now_rfc3339();
        let conn = self.conn.lock().map_err(|_| lock_err())?;
        conn.execute(
            r#"
            UPDATE requests
            SET status = ?2, resolution_json = ?3, reason = ?4, resolved_at = ?5
            WHERE request_id = ?1 AND status = 'pending'
            "#,
            params![
                request_id,
                enum_to_string(&status)?,
                json_string(&resolution)?,
                reason,
                resolved_at
            ],
        )
        .map_err(store_err)?;
        drop(conn);
        self.get_request(request_id)
    }

    pub fn cancel_pending_requests_for_turn(
        &self,
        turn_id: &str,
        reason: String,
    ) -> Result<Vec<PendingRequest>> {
        let resolved_at = now_rfc3339();
        let resolution = json!({
            "decision": RequestDecision::Cancel,
            "response": Value::Null,
        });
        let resolution_json = json_string(&resolution)?;
        let conn = self.conn.lock().map_err(|_| lock_err())?;
        let mut stmt = conn
            .prepare(
                r#"
                SELECT request_id, resource_uri, session_id, turn_id, kind, status, summary,
                       payload_json, resolution_json, reason, created_at, resolved_at
                FROM requests
                WHERE turn_id = ?1 AND status = 'pending'
                ORDER BY created_at ASC
                "#,
            )
            .map_err(store_err)?;
        let mut rows = stmt.query(params![turn_id]).map_err(store_err)?;
        let mut requests = Vec::new();
        while let Some(row) = rows.next().map_err(store_err)? {
            requests.push(request_row(row).map_err(store_err)?.try_into_request()?);
        }
        drop(rows);
        drop(stmt);
        for request in &mut requests {
            conn.execute(
                r#"
                UPDATE requests
                SET status = 'cancelled', resolution_json = ?2, reason = ?3, resolved_at = ?4
                WHERE request_id = ?1 AND status = 'pending'
                "#,
                params![request.request_id, resolution_json, reason, resolved_at],
            )
            .map_err(store_err)?;
            request.status = RequestStatus::Cancelled;
            request.resolution = Some(resolution.clone());
            request.reason = Some(reason.clone());
            request.resolved_at = Some(resolved_at.clone());
        }
        Ok(requests)
    }

    pub fn get_request(&self, request_id: &str) -> Result<PendingRequest> {
        let conn = self.conn.lock().map_err(|_| lock_err())?;
        let row = conn
            .query_row(
                r#"
                SELECT request_id, resource_uri, session_id, turn_id, kind, status, summary,
                       payload_json, resolution_json, reason, created_at, resolved_at
                FROM requests
                WHERE request_id = ?1
                "#,
                params![request_id],
                request_row,
            )
            .optional()
            .map_err(store_err)?
            .ok_or_else(|| SingletonError::NotFound {
                resource: "request",
                id: request_id.to_string(),
            })?;
        row.try_into_request()
    }

    pub fn unresolved_request_by_session_and_payload_key(
        &self,
        session_id: &str,
        payload_key: &str,
        payload_value: &str,
    ) -> Result<Option<PendingRequest>> {
        Ok(self.pending_requests()?.into_iter().find(|request| {
            request.session_id == session_id
                && request.payload.get(payload_key).and_then(Value::as_str) == Some(payload_value)
        }))
    }

    pub fn mark_interrupted_turns(&self) -> Result<Vec<Turn>> {
        let interrupted = self.active_turns_for_recovery()?;
        for turn in &interrupted {
            self.mark_turn_interrupted(
                &turn.turn_id,
                "turn interrupted by broker shutdown or restart",
            )?;
        }
        Ok(interrupted)
    }

    pub fn active_turns_for_recovery(&self) -> Result<Vec<Turn>> {
        let conn = self.conn.lock().map_err(|_| lock_err())?;
        let mut stmt = conn
            .prepare(
                r#"
                SELECT turn_id, resource_uri, session_id, backend_turn_id, message, status, unread, created_at, updated_at
                FROM turns
                WHERE status IN ('queued', 'running', 'needs_input')
                ORDER BY created_at ASC
                "#,
            )
            .map_err(store_err)?;
        let mut rows = stmt.query([]).map_err(store_err)?;
        let mut turns = Vec::new();
        while let Some(row) = rows.next().map_err(store_err)? {
            turns.push(turn_row(row).map_err(store_err)?.try_into_turn()?);
        }
        Ok(turns)
    }

    pub fn mark_turn_interrupted(&self, turn_id: &str, _summary: &str) -> Result<Turn> {
        let now = now_rfc3339();
        let conn = self.conn.lock().map_err(|_| lock_err())?;
        conn.execute(
            r#"
            UPDATE turns
            SET status = 'failed', unread = 1, updated_at = ?2
            WHERE turn_id = ?1 AND status IN ('queued', 'running', 'needs_input')
            "#,
            params![turn_id, now],
        )
        .map_err(store_err)?;
        drop(conn);
        self.get_turn(turn_id)
    }

    pub fn append_event(
        &self,
        resource_uri_value: &str,
        parent_resource_uri: Option<&str>,
        event_type: &str,
        origin_kind: &str,
        origin_id: &str,
        payload: Value,
    ) -> Result<Event> {
        let event_id = new_id("evt");
        let created_at = now_rfc3339();
        let payload_json = json_string(&payload)?;
        let conn = self.conn.lock().map_err(|_| lock_err())?;
        conn.execute(
            r#"
            INSERT INTO events
            (event_id, resource_uri, parent_resource_uri, event_type, origin_kind, origin_id, payload_json, created_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            "#,
            params![
                event_id,
                resource_uri_value,
                parent_resource_uri,
                event_type,
                origin_kind,
                origin_id,
                payload_json,
                created_at
            ],
        )
        .map_err(store_err)?;
        let server_seq = conn.last_insert_rowid();
        Ok(Event {
            event_id,
            server_seq,
            resource_uri: resource_uri_value.to_string(),
            parent_resource_uri: parent_resource_uri.map(ToString::to_string),
            event_type: event_type.to_string(),
            origin_kind: origin_kind.to_string(),
            origin_id: origin_id.to_string(),
            payload,
            created_at,
        })
    }

    pub fn read_events(
        &self,
        target_resource_uri: Option<&str>,
        cursor: i64,
        limit: usize,
        event_types: &[String],
    ) -> Result<Vec<Event>> {
        let limit = i64::try_from(limit.min(500)).map_err(|error| {
            SingletonError::InvalidInput(format!("invalid event limit conversion: {error}"))
        })?;
        let conn = self.conn.lock().map_err(|_| lock_err())?;
        let (sql, params_value): (&str, Vec<String>) = if let Some(target) = target_resource_uri {
            (
                r#"
                SELECT server_seq, event_id, resource_uri, parent_resource_uri, event_type,
                       origin_kind, origin_id, payload_json, created_at
                FROM events
                WHERE server_seq > ?1 AND (resource_uri = ?2 OR parent_resource_uri = ?2)
                ORDER BY server_seq ASC
                LIMIT ?3
                "#,
                vec![cursor.to_string(), target.to_string(), limit.to_string()],
            )
        } else {
            (
                r#"
                SELECT server_seq, event_id, resource_uri, parent_resource_uri, event_type,
                       origin_kind, origin_id, payload_json, created_at
                FROM events
                WHERE server_seq > ?1
                ORDER BY server_seq ASC
                LIMIT ?2
                "#,
                vec![cursor.to_string(), limit.to_string()],
            )
        };
        let mut stmt = conn.prepare(sql).map_err(store_err)?;
        let mut rows = if target_resource_uri.is_some() {
            stmt.query(params![params_value[0], params_value[1], params_value[2]])
                .map_err(store_err)?
        } else {
            stmt.query(params![params_value[0], params_value[1]])
                .map_err(store_err)?
        };
        let mut events = Vec::new();
        while let Some(row) = rows.next().map_err(store_err)? {
            let event = event_row(row).map_err(store_err)?.try_into_event()?;
            if event_types.is_empty() || event_types.contains(&event.event_type) {
                events.push(event);
            }
        }
        Ok(events)
    }

    pub fn read_recent_events_for_resource(
        &self,
        target_resource_uri: &str,
        limit: usize,
    ) -> Result<Vec<Event>> {
        let limit = i64::try_from(limit.min(500)).map_err(|error| {
            SingletonError::InvalidInput(format!("invalid event limit conversion: {error}"))
        })?;
        let conn = self.conn.lock().map_err(|_| lock_err())?;
        let mut stmt = conn
            .prepare(
                r#"
                SELECT server_seq, event_id, resource_uri, parent_resource_uri, event_type,
                       origin_kind, origin_id, payload_json, created_at
                FROM events
                WHERE resource_uri = ?1 OR parent_resource_uri = ?1
                ORDER BY server_seq DESC
                LIMIT ?2
                "#,
            )
            .map_err(store_err)?;
        let mut rows = stmt
            .query(params![target_resource_uri, limit])
            .map_err(store_err)?;
        let mut events = Vec::new();
        while let Some(row) = rows.next().map_err(store_err)? {
            events.push(event_row(row).map_err(store_err)?.try_into_event()?);
        }
        events.reverse();
        Ok(events)
    }

    pub fn active_session_count_for_workspace(&self, workspace_id: &str) -> Result<usize> {
        let conn = self.conn.lock().map_err(|_| lock_err())?;
        let count: i64 = conn
            .query_row(
                r#"
                SELECT COUNT(*)
                FROM sessions
                WHERE workspace_id = ?1
                  AND status NOT IN ('archived', 'disposed', 'deleted', 'completed', 'failed', 'cancelled')
                "#,
                params![workspace_id],
                |row| row.get(0),
            )
            .map_err(store_err)?;
        usize::try_from(count).map_err(|error| {
            SingletonError::Store(format!("invalid active session count conversion: {error}"))
        })
    }

    pub fn close_session(&self, session_id: &str, disposition: CloseDisposition) -> Result<()> {
        let status = match disposition {
            CloseDisposition::Archive => ResourceStatus::Archived,
            CloseDisposition::Dispose => ResourceStatus::Disposed,
            CloseDisposition::Delete => ResourceStatus::Deleted,
        };
        self.update_session_status(session_id, status, None)
    }

    pub fn upsert_remote_host_health(&self, health: &RemoteHostHealth) -> Result<()> {
        let conn = self.conn.lock().map_err(|_| lock_err())?;
        conn.execute(
            r#"
            INSERT INTO remote_hosts
            (host_id, state, remote_identity_json, capabilities_json, last_checked_at,
             last_success_at, last_error, updated_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            ON CONFLICT(host_id) DO UPDATE SET
                state = excluded.state,
                remote_identity_json = excluded.remote_identity_json,
                capabilities_json = excluded.capabilities_json,
                last_checked_at = excluded.last_checked_at,
                last_success_at = excluded.last_success_at,
                last_error = excluded.last_error,
                updated_at = excluded.updated_at
            "#,
            params![
                health.host_id,
                enum_to_string(&health.state)?,
                opt_json(&health.remote_identity)?,
                opt_json(&health.capabilities)?,
                health.last_checked_at,
                health.last_success_at,
                health.last_error,
                health.updated_at
            ],
        )
        .map_err(store_err)?;
        Ok(())
    }

    pub fn get_remote_host_health(&self, host_id: &str) -> Result<Option<RemoteHostHealth>> {
        let conn = self.conn.lock().map_err(|_| lock_err())?;
        let row = conn
            .query_row(
                r#"
                SELECT host_id, state, remote_identity_json, capabilities_json, last_checked_at,
                       last_success_at, last_error, updated_at
                FROM remote_hosts
                WHERE host_id = ?1
                "#,
                params![host_id],
                remote_host_row,
            )
            .optional()
            .map_err(store_err)?;
        row.map(RemoteHostRow::try_into_health).transpose()
    }

    pub fn list_remote_host_health(&self) -> Result<Vec<RemoteHostHealth>> {
        let conn = self.conn.lock().map_err(|_| lock_err())?;
        let mut stmt = conn
            .prepare(
                r#"
                SELECT host_id, state, remote_identity_json, capabilities_json, last_checked_at,
                       last_success_at, last_error, updated_at
                FROM remote_hosts
                ORDER BY host_id ASC
                "#,
            )
            .map_err(store_err)?;
        let mut rows = stmt.query([]).map_err(store_err)?;
        let mut health = Vec::new();
        while let Some(row) = rows.next().map_err(store_err)? {
            health.push(remote_host_row(row).map_err(store_err)?.try_into_health()?);
        }
        Ok(health)
    }

    pub fn upsert_remote_resource_link(&self, link: &RemoteResourceLink) -> Result<()> {
        let conn = self.conn.lock().map_err(|_| lock_err())?;
        conn.execute(
            r#"
            INSERT INTO remote_resource_links
            (local_resource_uri, local_resource_kind, local_id, host_id, remote_resource_uri,
             remote_id, remote_cursor, created_at, updated_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
            ON CONFLICT(local_resource_uri) DO UPDATE SET
                local_resource_kind = excluded.local_resource_kind,
                local_id = excluded.local_id,
                host_id = excluded.host_id,
                remote_resource_uri = excluded.remote_resource_uri,
                remote_id = excluded.remote_id,
                remote_cursor = excluded.remote_cursor,
                updated_at = excluded.updated_at
            "#,
            params![
                link.local_resource_uri,
                enum_to_string(&link.local_resource_kind)?,
                link.local_id,
                link.host_id,
                link.remote_resource_uri,
                link.remote_id,
                link.remote_cursor,
                link.created_at,
                link.updated_at
            ],
        )
        .map_err(store_err)?;
        Ok(())
    }

    pub fn get_remote_resource_link(
        &self,
        local_resource_uri: &str,
    ) -> Result<Option<RemoteResourceLink>> {
        let conn = self.conn.lock().map_err(|_| lock_err())?;
        let row = conn
            .query_row(
                r#"
                SELECT local_resource_uri, local_resource_kind, local_id, host_id,
                       remote_resource_uri, remote_id, remote_cursor, created_at, updated_at
                FROM remote_resource_links
                WHERE local_resource_uri = ?1
                "#,
                params![local_resource_uri],
                remote_link_row,
            )
            .optional()
            .map_err(store_err)?;
        row.map(RemoteResourceLinkRow::try_into_link).transpose()
    }

    pub fn get_remote_resource_link_by_remote(
        &self,
        host_id: &str,
        remote_resource_uri: &str,
    ) -> Result<Option<RemoteResourceLink>> {
        let conn = self.conn.lock().map_err(|_| lock_err())?;
        let row = conn
            .query_row(
                r#"
                SELECT local_resource_uri, local_resource_kind, local_id, host_id,
                       remote_resource_uri, remote_id, remote_cursor, created_at, updated_at
                FROM remote_resource_links
                WHERE host_id = ?1 AND remote_resource_uri = ?2
                "#,
                params![host_id, remote_resource_uri],
                remote_link_row,
            )
            .optional()
            .map_err(store_err)?;
        row.map(RemoteResourceLinkRow::try_into_link).transpose()
    }

    pub fn remote_resource_links_for_host(&self, host_id: &str) -> Result<Vec<RemoteResourceLink>> {
        let conn = self.conn.lock().map_err(|_| lock_err())?;
        let mut stmt = conn
            .prepare(
                r#"
                SELECT local_resource_uri, local_resource_kind, local_id, host_id,
                       remote_resource_uri, remote_id, remote_cursor, created_at, updated_at
                FROM remote_resource_links
                WHERE host_id = ?1
                ORDER BY created_at ASC
                "#,
            )
            .map_err(store_err)?;
        let mut rows = stmt.query(params![host_id]).map_err(store_err)?;
        let mut links = Vec::new();
        while let Some(row) = rows.next().map_err(store_err)? {
            links.push(remote_link_row(row).map_err(store_err)?.try_into_link()?);
        }
        Ok(links)
    }

    pub fn update_remote_resource_cursor(
        &self,
        local_resource_uri: &str,
        remote_cursor: i64,
    ) -> Result<()> {
        let now = now_rfc3339();
        let conn = self.conn.lock().map_err(|_| lock_err())?;
        conn.execute(
            r#"
            UPDATE remote_resource_links
            SET remote_cursor = ?2, updated_at = ?3
            WHERE local_resource_uri = ?1
            "#,
            params![local_resource_uri, remote_cursor, now],
        )
        .map_err(store_err)?;
        Ok(())
    }

    pub fn upsert_forwarded_operation(&self, operation: &ForwardedOperation) -> Result<()> {
        let conn = self.conn.lock().map_err(|_| lock_err())?;
        conn.execute(
            r#"
            INSERT INTO forwarded_operations
            (operation_id, host_id, operation_kind, status, local_resource_uri, request_json,
             result_json, error, created_at, updated_at)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
            ON CONFLICT(operation_id) DO UPDATE SET
                host_id = excluded.host_id,
                operation_kind = excluded.operation_kind,
                status = excluded.status,
                local_resource_uri = excluded.local_resource_uri,
                request_json = excluded.request_json,
                result_json = excluded.result_json,
                error = excluded.error,
                updated_at = excluded.updated_at
            "#,
            params![
                operation.operation_id,
                operation.host_id,
                operation.operation_kind,
                enum_to_string(&operation.status)?,
                operation.local_resource_uri,
                json_string(&operation.request)?,
                opt_json(&operation.result)?,
                operation.error,
                operation.created_at,
                operation.updated_at
            ],
        )
        .map_err(store_err)?;
        Ok(())
    }

    pub fn get_forwarded_operation(
        &self,
        operation_id: &str,
    ) -> Result<Option<ForwardedOperation>> {
        let conn = self.conn.lock().map_err(|_| lock_err())?;
        let row = conn
            .query_row(
                r#"
                SELECT operation_id, host_id, operation_kind, status, local_resource_uri,
                       request_json, result_json, error, created_at, updated_at
                FROM forwarded_operations
                WHERE operation_id = ?1
                "#,
                params![operation_id],
                forwarded_operation_row,
            )
            .optional()
            .map_err(store_err)?;
        row.map(ForwardedOperationRow::try_into_operation)
            .transpose()
    }

    pub fn pending_forwarded_operations_for_host(
        &self,
        host_id: &str,
    ) -> Result<Vec<ForwardedOperation>> {
        let conn = self.conn.lock().map_err(|_| lock_err())?;
        let mut stmt = conn
            .prepare(
                r#"
                SELECT operation_id, host_id, operation_kind, status, local_resource_uri,
                       request_json, result_json, error, created_at, updated_at
                FROM forwarded_operations
                WHERE host_id = ?1 AND status IN ('pending', 'uncertain')
                ORDER BY created_at ASC
                "#,
            )
            .map_err(store_err)?;
        let mut rows = stmt.query(params![host_id]).map_err(store_err)?;
        let mut operations = Vec::new();
        while let Some(row) = rows.next().map_err(store_err)? {
            operations.push(
                forwarded_operation_row(row)
                    .map_err(store_err)?
                    .try_into_operation()?,
            );
        }
        Ok(operations)
    }

    fn put_resource_state<T: Serialize>(
        &self,
        resource_uri_value: &str,
        kind: ResourceKind,
        state: &T,
    ) -> Result<()> {
        let conn = self.conn.lock().map_err(|_| lock_err())?;
        conn.execute(
            r#"
            INSERT OR REPLACE INTO resource_states
            (resource_uri, resource_kind, state_json, updated_at)
            VALUES (?1, ?2, ?3, ?4)
            "#,
            params![
                resource_uri_value,
                kind.resource_name(),
                json_string(state)?,
                now_rfc3339()
            ],
        )
        .map_err(store_err)?;
        Ok(())
    }
}

struct WorkspaceRow {
    workspace_id: String,
    resource_uri: String,
    host_id: String,
    status: String,
    path: Option<String>,
    repo_json: Option<String>,
    cleanup_policy: String,
    created_at: String,
}

impl WorkspaceRow {
    fn try_into_workspace(self) -> Result<Workspace> {
        Ok(Workspace {
            workspace_id: self.workspace_id,
            resource_uri: self.resource_uri,
            host_id: self.host_id,
            status: enum_from_string(&self.status)?,
            path: self.path,
            repo: opt_from_json(self.repo_json)?,
            cleanup_policy: enum_from_string(&self.cleanup_policy)?,
            created_at: self.created_at,
        })
    }
}

struct SessionRow {
    session_id: String,
    resource_uri: String,
    title: String,
    description: Option<String>,
    backend: String,
    backend_session_id: Option<String>,
    workspace_id: Option<String>,
    status: String,
    latest_event_cursor: i64,
    labels_json: String,
    created_at: String,
    updated_at: String,
}

impl SessionRow {
    fn try_into_session(self) -> Result<Session> {
        Ok(Session {
            session_id: self.session_id,
            resource_uri: self.resource_uri,
            title: self.title,
            description: self.description,
            backend: self.backend,
            backend_session_id: self.backend_session_id,
            workspace_id: self.workspace_id,
            status: enum_from_string(&self.status)?,
            latest_event_cursor: self.latest_event_cursor,
            labels: from_json(&self.labels_json)?,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

struct TurnRow {
    turn_id: String,
    resource_uri: String,
    session_id: String,
    backend_turn_id: Option<String>,
    message: String,
    status: String,
    unread: i64,
    created_at: String,
    updated_at: String,
}

impl TurnRow {
    fn try_into_turn(self) -> Result<Turn> {
        Ok(Turn {
            turn_id: self.turn_id,
            resource_uri: self.resource_uri,
            session_id: self.session_id,
            backend_turn_id: self.backend_turn_id,
            message: self.message,
            status: enum_from_string(&self.status)?,
            unread: self.unread != 0,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

struct RequestRow {
    request_id: String,
    resource_uri: String,
    session_id: String,
    turn_id: Option<String>,
    kind: String,
    status: String,
    summary: String,
    payload_json: String,
    resolution_json: Option<String>,
    reason: Option<String>,
    created_at: String,
    resolved_at: Option<String>,
}

impl RequestRow {
    fn try_into_request(self) -> Result<PendingRequest> {
        Ok(PendingRequest {
            request_id: self.request_id,
            resource_uri: self.resource_uri,
            session_id: self.session_id,
            turn_id: self.turn_id,
            kind: enum_from_string(&self.kind)?,
            status: enum_from_string(&self.status)?,
            summary: self.summary,
            payload: from_json(&self.payload_json)?,
            resolution: opt_from_json(self.resolution_json)?,
            reason: self.reason,
            created_at: self.created_at,
            resolved_at: self.resolved_at,
        })
    }
}

struct EventRow {
    server_seq: i64,
    event_id: String,
    resource_uri: String,
    parent_resource_uri: Option<String>,
    event_type: String,
    origin_kind: String,
    origin_id: String,
    payload_json: String,
    created_at: String,
}

impl EventRow {
    fn try_into_event(self) -> Result<Event> {
        Ok(Event {
            event_id: self.event_id,
            server_seq: self.server_seq,
            resource_uri: self.resource_uri,
            parent_resource_uri: self.parent_resource_uri,
            event_type: self.event_type,
            origin_kind: self.origin_kind,
            origin_id: self.origin_id,
            payload: from_json(&self.payload_json)?,
            created_at: self.created_at,
        })
    }
}

struct RemoteHostRow {
    host_id: String,
    state: String,
    remote_identity_json: Option<String>,
    capabilities_json: Option<String>,
    last_checked_at: Option<String>,
    last_success_at: Option<String>,
    last_error: Option<String>,
    updated_at: String,
}

impl RemoteHostRow {
    fn try_into_health(self) -> Result<RemoteHostHealth> {
        Ok(RemoteHostHealth {
            host_id: self.host_id,
            state: enum_from_string(&self.state)?,
            remote_identity: opt_from_json::<RemoteBrokerIdentity>(self.remote_identity_json)?,
            capabilities: opt_from_json(self.capabilities_json)?,
            last_checked_at: self.last_checked_at,
            last_success_at: self.last_success_at,
            last_error: self.last_error,
            updated_at: self.updated_at,
        })
    }
}

struct RemoteResourceLinkRow {
    local_resource_uri: String,
    local_resource_kind: String,
    local_id: String,
    host_id: String,
    remote_resource_uri: String,
    remote_id: String,
    remote_cursor: i64,
    created_at: String,
    updated_at: String,
}

impl RemoteResourceLinkRow {
    fn try_into_link(self) -> Result<RemoteResourceLink> {
        Ok(RemoteResourceLink {
            local_resource_uri: self.local_resource_uri,
            local_resource_kind: enum_from_string(&self.local_resource_kind)?,
            local_id: self.local_id,
            host_id: self.host_id,
            remote_resource_uri: self.remote_resource_uri,
            remote_id: self.remote_id,
            remote_cursor: self.remote_cursor,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

struct ForwardedOperationRow {
    operation_id: String,
    host_id: String,
    operation_kind: String,
    status: String,
    local_resource_uri: Option<String>,
    request_json: String,
    result_json: Option<String>,
    error: Option<String>,
    created_at: String,
    updated_at: String,
}

impl ForwardedOperationRow {
    fn try_into_operation(self) -> Result<ForwardedOperation> {
        Ok(ForwardedOperation {
            operation_id: self.operation_id,
            host_id: self.host_id,
            operation_kind: self.operation_kind,
            status: enum_from_string::<ForwardedOperationStatus>(&self.status)?,
            local_resource_uri: self.local_resource_uri,
            request: from_json(&self.request_json)?,
            result: opt_from_json(self.result_json)?,
            error: self.error,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

fn session_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SessionRow> {
    Ok(SessionRow {
        session_id: row.get(0)?,
        resource_uri: row.get(1)?,
        title: row.get(2)?,
        description: row.get(3)?,
        backend: row.get(4)?,
        backend_session_id: row.get(5)?,
        workspace_id: row.get(6)?,
        status: row.get(7)?,
        latest_event_cursor: row.get(8)?,
        labels_json: row.get(9)?,
        created_at: row.get(10)?,
        updated_at: row.get(11)?,
    })
}

fn turn_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<TurnRow> {
    Ok(TurnRow {
        turn_id: row.get(0)?,
        resource_uri: row.get(1)?,
        session_id: row.get(2)?,
        backend_turn_id: row.get(3)?,
        message: row.get(4)?,
        status: row.get(5)?,
        unread: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
    })
}

fn request_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RequestRow> {
    Ok(RequestRow {
        request_id: row.get(0)?,
        resource_uri: row.get(1)?,
        session_id: row.get(2)?,
        turn_id: row.get(3)?,
        kind: row.get(4)?,
        status: row.get(5)?,
        summary: row.get(6)?,
        payload_json: row.get(7)?,
        resolution_json: row.get(8)?,
        reason: row.get(9)?,
        created_at: row.get(10)?,
        resolved_at: row.get(11)?,
    })
}

fn event_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<EventRow> {
    Ok(EventRow {
        server_seq: row.get(0)?,
        event_id: row.get(1)?,
        resource_uri: row.get(2)?,
        parent_resource_uri: row.get(3)?,
        event_type: row.get(4)?,
        origin_kind: row.get(5)?,
        origin_id: row.get(6)?,
        payload_json: row.get(7)?,
        created_at: row.get(8)?,
    })
}

fn remote_host_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RemoteHostRow> {
    Ok(RemoteHostRow {
        host_id: row.get(0)?,
        state: row.get(1)?,
        remote_identity_json: row.get(2)?,
        capabilities_json: row.get(3)?,
        last_checked_at: row.get(4)?,
        last_success_at: row.get(5)?,
        last_error: row.get(6)?,
        updated_at: row.get(7)?,
    })
}

fn remote_link_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RemoteResourceLinkRow> {
    Ok(RemoteResourceLinkRow {
        local_resource_uri: row.get(0)?,
        local_resource_kind: row.get(1)?,
        local_id: row.get(2)?,
        host_id: row.get(3)?,
        remote_resource_uri: row.get(4)?,
        remote_id: row.get(5)?,
        remote_cursor: row.get(6)?,
        created_at: row.get(7)?,
        updated_at: row.get(8)?,
    })
}

fn forwarded_operation_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ForwardedOperationRow> {
    Ok(ForwardedOperationRow {
        operation_id: row.get(0)?,
        host_id: row.get(1)?,
        operation_kind: row.get(2)?,
        status: row.get(3)?,
        local_resource_uri: row.get(4)?,
        request_json: row.get(5)?,
        result_json: row.get(6)?,
        error: row.get(7)?,
        created_at: row.get(8)?,
        updated_at: row.get(9)?,
    })
}

fn status_to_string(status: &ResourceStatus) -> Result<String> {
    enum_to_string(status)
}

fn enum_to_string<T: Serialize>(value: &T) -> Result<String> {
    let json = serde_json::to_value(value)
        .map_err(|error| SingletonError::Store(format!("serialize enum: {error}")))?;
    json.as_str()
        .map(ToString::to_string)
        .ok_or_else(|| SingletonError::Store("enum did not serialize to string".to_string()))
}

fn enum_from_string<T: DeserializeOwned>(value: &str) -> Result<T> {
    serde_json::from_value(Value::String(value.to_string()))
        .map_err(|error| SingletonError::Store(format!("deserialize enum '{value}': {error}")))
}

fn json_string<T: Serialize>(value: &T) -> Result<String> {
    serde_json::to_string(value)
        .map_err(|error| SingletonError::Store(format!("serialize json: {error}")))
}

fn opt_json<T: Serialize>(value: &Option<T>) -> Result<Option<String>> {
    value.as_ref().map(json_string).transpose()
}

fn from_json<T: DeserializeOwned>(value: &str) -> Result<T> {
    serde_json::from_str(value)
        .map_err(|error| SingletonError::Store(format!("deserialize json: {error}")))
}

fn opt_from_json<T: DeserializeOwned>(value: Option<String>) -> Result<Option<T>> {
    value.map(|json| from_json(&json)).transpose()
}

fn store_err(error: rusqlite::Error) -> SingletonError {
    SingletonError::Store(error.to_string())
}

fn lock_err() -> SingletonError {
    SingletonError::Store("store lock poisoned".to_string())
}

fn bool_to_i64(value: bool) -> i64 {
    if value { 1 } else { 0 }
}

pub fn new_session(title: String, backend: String, workspace_id: Option<WorkspaceId>) -> Session {
    let session_id = new_id("sess");
    let now = now_rfc3339();
    Session {
        resource_uri: resource_uri(ResourceKind::Session, &session_id),
        session_id,
        title,
        description: None,
        backend,
        backend_session_id: None,
        workspace_id,
        status: ResourceStatus::Pending,
        latest_event_cursor: 0,
        labels: Vec::new(),
        created_at: now.clone(),
        updated_at: now,
    }
}

pub fn new_turn(session_id: SessionId, message: String) -> Turn {
    let turn_id = new_id("turn");
    let now = now_rfc3339();
    Turn {
        resource_uri: resource_uri(ResourceKind::Turn, &turn_id),
        turn_id,
        session_id,
        backend_turn_id: None,
        message,
        status: ResourceStatus::Pending,
        unread: false,
        created_at: now.clone(),
        updated_at: now,
    }
}

pub fn new_request(
    session_id: SessionId,
    turn_id: Option<TurnId>,
    kind: RequestKind,
    summary: String,
    payload: Value,
) -> PendingRequest {
    let request_id = new_id("req");
    PendingRequest {
        resource_uri: resource_uri(ResourceKind::Request, &request_id),
        request_id,
        session_id,
        turn_id,
        kind,
        status: RequestStatus::Pending,
        summary,
        payload,
        resolution: None,
        reason: None,
        created_at: now_rfc3339(),
        resolved_at: None,
    }
}

pub fn workspace_from_path(path: String) -> Workspace {
    let workspace_id = new_id("work");
    Workspace {
        resource_uri: resource_uri(ResourceKind::Workspace, &workspace_id),
        workspace_id,
        host_id: singleton_core::LOCAL_HOST_ID.to_string(),
        status: ResourceStatus::Ready,
        path: Some(path),
        repo: None,
        cleanup_policy: singleton_core::CleanupPolicy::Keep,
        created_at: now_rfc3339(),
    }
}

pub fn workspace_from_repo(path: String, repo: RepoMetadata) -> Workspace {
    let workspace_id = new_id("work");
    Workspace {
        resource_uri: resource_uri(ResourceKind::Workspace, &workspace_id),
        workspace_id,
        host_id: singleton_core::LOCAL_HOST_ID.to_string(),
        status: ResourceStatus::Ready,
        path: Some(path),
        repo: Some(repo),
        cleanup_policy: singleton_core::CleanupPolicy::Keep,
        created_at: now_rfc3339(),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use singleton_core::{
        CleanupPolicy, ForwardedOperationStatus, HostConnectionState, RemoteBrokerIdentity,
        RemoteHostHealth, RemoteResourceLink, ResourceKind, Workspace,
    };
    use tempfile::NamedTempFile;

    use super::*;

    #[test]
    fn migrations_apply_to_empty_database() -> Result<()> {
        let tmp = NamedTempFile::new()
            .map_err(|error| SingletonError::Store(format!("create temp db: {error}")))?;
        let store = Store::open(tmp.path())?;

        let event = store.append_event(
            "singleton-root://",
            None,
            "host.available",
            "singleton",
            "test",
            json!({ "ok": true }),
        )?;

        assert_eq!(event.server_seq, 1);
        Ok(())
    }

    #[test]
    fn workspace_session_turn_roundtrip() -> Result<()> {
        let store = Store::open_memory()?;
        let workspace = Workspace {
            workspace_id: "work_test".to_string(),
            resource_uri: resource_uri(ResourceKind::Workspace, "work_test"),
            host_id: singleton_core::LOCAL_HOST_ID.to_string(),
            status: ResourceStatus::Ready,
            path: Some("/tmp/work".to_string()),
            repo: None,
            cleanup_policy: CleanupPolicy::Keep,
            created_at: now_rfc3339(),
        };
        store.insert_workspace(&workspace)?;

        let session = new_session(
            "Test session".to_string(),
            singleton_core::FAKE_BACKEND_ID.to_string(),
            Some(workspace.workspace_id.clone()),
        );
        store.insert_session(&session)?;
        let turn = new_turn(session.session_id.clone(), "hello".to_string());
        store.insert_turn(&turn)?;

        assert_eq!(
            store.get_workspace(&workspace.workspace_id)?.path,
            workspace.path
        );
        assert_eq!(
            store.get_session(&session.session_id)?.title,
            "Test session"
        );
        assert!(
            store
                .active_turn_for_session(&session.session_id)?
                .is_none()
        );
        Ok(())
    }

    #[test]
    fn event_cursor_filters_by_parent_resource() -> Result<()> {
        let store = Store::open_memory()?;
        let session_uri = resource_uri(ResourceKind::Session, "sess_test");
        let turn_uri = resource_uri(ResourceKind::Turn, "turn_test");

        store.append_event(
            &turn_uri,
            Some(&session_uri),
            "turn.started",
            "backend",
            "fake",
            json!({}),
        )?;
        store.append_event(
            "singleton-session:/other",
            None,
            "session.created",
            "singleton",
            "test",
            json!({}),
        )?;

        let events = store.read_events(Some(&session_uri), 0, 100, &[])?;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "turn.started");
        Ok(())
    }

    #[test]
    fn request_resolution_is_idempotently_readable() -> Result<()> {
        let store = Store::open_memory()?;
        let request = new_request(
            "sess_test".to_string(),
            Some("turn_test".to_string()),
            RequestKind::Permission,
            "Allow command?".to_string(),
            json!({ "tool": "bash" }),
        );
        store.insert_request(&request)?;

        let resolved = store.resolve_request(
            &request.request_id,
            RequestDecision::Deny,
            Some(json!({ "decision": "deny" })),
            Some("unsafe".to_string()),
        )?;

        assert_eq!(resolved.status, RequestStatus::Resolved);
        assert_eq!(store.pending_requests()?.len(), 0);
        Ok(())
    }

    #[test]
    fn remote_federation_state_roundtrips() -> Result<()> {
        let store = Store::open_memory()?;
        let now = now_rfc3339();
        let health = RemoteHostHealth {
            host_id: "devbox".to_string(),
            state: HostConnectionState::Available,
            remote_identity: Some(RemoteBrokerIdentity {
                broker_id: "remote-state-1".to_string(),
                protocol_version: "0.1".to_string(),
            }),
            capabilities: None,
            last_checked_at: Some(now.clone()),
            last_success_at: Some(now.clone()),
            last_error: None,
            updated_at: now.clone(),
        };
        store.upsert_remote_host_health(&health)?;
        assert_eq!(
            store.get_remote_host_health("devbox")?.map(|h| h.state),
            Some(HostConnectionState::Available)
        );

        let link = RemoteResourceLink {
            local_resource_uri: resource_uri(ResourceKind::Session, "sess_local"),
            local_resource_kind: ResourceKind::Session,
            local_id: "sess_local".to_string(),
            host_id: "devbox".to_string(),
            remote_resource_uri: resource_uri(ResourceKind::Session, "sess_remote"),
            remote_id: "sess_remote".to_string(),
            remote_cursor: 7,
            created_at: now.clone(),
            updated_at: now.clone(),
        };
        store.upsert_remote_resource_link(&link)?;
        store.update_remote_resource_cursor(&link.local_resource_uri, 9)?;
        let stored_link = store
            .get_remote_resource_link(&link.local_resource_uri)?
            .ok_or_else(|| SingletonError::Store("remote link missing".to_string()))?;
        assert_eq!(stored_link.remote_cursor, 9);
        assert_eq!(store.remote_resource_links_for_host("devbox")?.len(), 1);

        let mut operation = ForwardedOperation::pending(
            "op_create",
            "devbox",
            "create_session",
            json!({ "description": "remote" }),
        );
        store.upsert_forwarded_operation(&operation)?;
        operation.status = ForwardedOperationStatus::Applied;
        operation.local_resource_uri = Some(link.local_resource_uri.clone());
        operation.result = Some(json!({ "remote_session_id": "sess_remote" }));
        operation.updated_at = now_rfc3339();
        store.upsert_forwarded_operation(&operation)?;
        let stored_operation = store
            .get_forwarded_operation("op_create")?
            .ok_or_else(|| SingletonError::Store("forwarded operation missing".to_string()))?;
        assert_eq!(stored_operation.status, ForwardedOperationStatus::Applied);
        assert!(
            store
                .pending_forwarded_operations_for_host("devbox")?
                .is_empty()
        );
        Ok(())
    }
}
