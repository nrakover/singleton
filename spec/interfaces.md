# singleton - Interface Specification

## 1. Scope

This file defines the public and internal contracts for the Rust MCP session
broker. It supersedes the prior Python/FastMCP daemon, hub, worker, hook, and
TUI interfaces.

The MVP public interface is an MCP server. The CLI is an admin/client utility,
not the primary product UX.

---

## 2. Resource Identity

Every durable entity has both an ordinary id and a stable resource URI. MCP
tool responses may use concise ids, but the store and event model must retain
URIs so future AHP adapters do not require a migration.

URI forms:

| Resource | URI |
|---|---|
| root | `singleton-root://` |
| host | `singleton-host:/<host_id>` |
| workspace | `singleton-workspace:/<workspace_id>` |
| session | `singleton-session:/<session_id>` |
| chat | `singleton-chat:/<chat_id>` |
| turn | `singleton-turn:/<turn_id>` |
| request | `singleton-request:/<request_id>` |
| changeset | `singleton-changeset:/<changeset_id>` |
| terminal | `singleton-terminal:/<terminal_id>` |
| artifact | `singleton-artifact:/<artifact_id>` |

Ids must be stable across daemon restarts. Prefer UUIDv7 or another
time-sortable random id format during implementation.

---

## 3. Core Rust Traits

These traits are conceptual contracts. Exact signatures can change during the
Rust spike to fit chosen crates, but the boundaries should remain.

### 3.1 Control surface

```rust
#[async_trait::async_trait]
pub trait ControlSurface {
    async fn serve(&self, broker: BrokerHandle) -> Result<(), SingletonError>;
}
```

First implementation: `McpControlSurface`.

Future optional implementation: `AhpControlSurface` for downstream clients
that want AHP-style resource subscriptions.

### 3.2 Host connector

```rust
#[async_trait::async_trait]
pub trait HostConnector {
    async fn connect(&self, config: HostConfig) -> Result<Box<dyn HostConnection>, SingletonError>;
}

#[async_trait::async_trait]
pub trait HostConnection: Send + Sync {
    fn host_id(&self) -> HostId;
    fn capabilities(&self) -> HostCapabilities;
    async fn ensure_workspace(&self, spec: WorkspaceSpec) -> Result<Workspace, SingletonError>;
    async fn close_workspace(&self, id: WorkspaceId, disposition: CloseDisposition, force: bool) -> Result<CleanupSummary, SingletonError>;
}
```

First implementation: `LocalHostConnector`.

Future implementations: `SshHostConnector`, cloud sandbox connectors,
`AhpHostConnector`.

### 3.3 Agent backend

```rust
#[async_trait::async_trait]
pub trait AgentBackend: Send + Sync {
    fn backend_id(&self) -> BackendId;
    fn capabilities(&self) -> BackendCapabilities;

    async fn create_session(&self, config: BackendSessionConfig) -> Result<BackendSession, SingletonError>;
    async fn resume_session(&self, id: BackendSessionId) -> Result<BackendSession, SingletonError>;
    async fn send_message(&self, session: &BackendSession, message: BackendMessage) -> Result<BackendTurn, SingletonError>;
    async fn cancel_turn(&self, session: &BackendSession, turn: BackendTurnId) -> Result<(), SingletonError>;
    fn events(&self, session: &BackendSession) -> BackendEventStream;
}
```

First real backend: GitHub Copilot SDK.

Required test backend: deterministic fake backend.

---

## 4. MCP Default Tool Contracts

All tools return typed JSON objects. Errors should be explicit MCP errors with
machine-readable codes and human-readable messages.

### 4.1 `get_capabilities`

Purpose: compact discovery for foreground agents.

Input:

```json
{}
```

Output:

```json
{
  "protocol_version": "0.1",
  "default_profile": "mvp",
  "hosts": [
    {
      "host_id": "host_local",
      "kind": "local",
      "status": "available",
      "workspace_providers": ["local_path", "git_worktree", "backend_default"],
      "agent_backends": ["copilot"]
    }
  ],
  "backends": [
    {
      "backend_id": "copilot",
      "display_name": "GitHub Copilot",
      "supports_resume": true,
      "supports_cancel": true,
      "supports_permissions": true
    }
  ],
  "limits": {
    "max_event_limit": 500,
    "max_wait_ms": 30000
  }
}
```

### 4.2 `get_inbox`

Purpose: fan in all actionable session-management items.

Input:

```json
{
  "filter": {
    "session_ids": ["sess_..."],
    "kinds": ["permission_request", "input_request", "failed_turn", "completed_turn", "stale_session"],
    "unread_only": true
  }
}
```

Output:

```json
{
  "counts": {
    "permission_request": 1,
    "input_request": 0,
    "failed_turn": 0,
    "completed_turn": 2,
    "stale_session": 0
  },
  "items": [
    {
      "kind": "permission_request",
      "request_id": "req_...",
      "session_id": "sess_...",
      "turn_id": "turn_...",
      "summary": "Allow command `cargo test --workspace --all-targets`?",
      "created_at": "2026-01-01T00:00:00Z"
    }
  ]
}
```

Inbox items must be concise. Detailed payloads should be read with
`get_session` or `read_events`.

### 4.3 `ensure_workspace`

Purpose: create or resolve a workspace.

Input:

```json
{
  "spec": {
    "kind": "git_worktree",
    "repo": "/path/to/repo",
    "base_ref": "main",
    "branch": "topic-branch",
    "create_branch": true,
    "worktree_path_hint": "/path/to/worktrees/topic",
    "host_id": "host_local",
    "cleanup_policy": "keep"
  }
}
```

Supported `spec.kind` values:

- `existing_workspace`
- `local_path`
- `git_worktree`
- `backend_default`

Output:

```json
{
  "workspace_id": "work_...",
  "resource_uri": "singleton-workspace:/work_...",
  "status": "ready",
  "host_id": "host_local",
  "path": "/path/to/worktree",
  "repo": {
    "root": "/path/to/repo",
    "base_ref": "main",
    "branch": "topic-branch"
  }
}
```

### 4.4 `create_session`

Purpose: create a durable background session.

Input:

```json
{
  "description": "Implement parser tests",
  "backend": "copilot",
  "workspace": {
    "kind": "git_worktree",
    "repo": "/path/to/repo",
    "base_ref": "main",
    "cleanup_policy": "delete_on_success"
  },
  "model": "auto",
  "mode": "autopilot",
  "permissions": {
    "default": "ask"
  },
  "labels": ["parser", "tests"]
}
```

Output:

```json
{
  "session_id": "sess_...",
  "resource_uri": "singleton-session:/sess_...",
  "workspace_id": "work_...",
  "status": "idle",
  "event_cursor": "42"
}
```

If `workspace` is inline, it must be resolved through the same path as
`ensure_workspace`.

### 4.5 `send_message`

Purpose: enqueue/start an asynchronous turn.

Input:

```json
{
  "session_id": "sess_...",
  "message": "Add tests for invalid parser inputs.",
  "mode": "autopilot",
  "workspace_override": null
}
```

Output:

```json
{
  "turn_id": "turn_...",
  "resource_uri": "singleton-turn:/turn_...",
  "status": "running",
  "event_cursor": "43"
}
```

The primitive is asynchronous. Foreground agents should poll or long-poll with
`read_events`.

### 4.6 `read_events`

Purpose: read sequence-numbered events for any resource.

Input:

```json
{
  "target": {
    "session_id": "sess_..."
  },
  "cursor": "43",
  "limit": 100,
  "event_types": ["turn.started", "message.delta", "turn.completed"],
  "wait_ms": 30000
}
```

Output:

```json
{
  "events": [
    {
      "event_id": "evt_...",
      "server_seq": 44,
      "resource_uri": "singleton-turn:/turn_...",
      "parent_resource_uri": "singleton-session:/sess_...",
      "event_type": "turn.completed",
      "origin_kind": "backend",
      "origin_id": "copilot",
      "payload": {
        "summary": "Added parser tests."
      },
      "created_at": "2026-01-01T00:00:00Z"
    }
  ],
  "next_cursor": "44",
  "timed_out": false
}
```

`wait_ms` replaces a separate wait tool.

### 4.7 `list_sessions`

Purpose: recover coordination state after context loss.

Input:

```json
{
  "filter": {
    "statuses": ["idle", "running", "needs_input"],
    "labels": ["parser"],
    "workspace_id": "work_..."
  },
  "limit": 50
}
```

Output:

```json
{
  "sessions": [
    {
      "session_id": "sess_...",
      "title": "Parser tests",
      "status": "running",
      "workspace_id": "work_...",
      "backend": "copilot",
      "latest_event_cursor": "44",
      "needs_input": false
    }
  ]
}
```

### 4.8 `get_session`

Purpose: inspect one session.

Input:

```json
{
  "session_id": "sess_..."
}
```

Output:

```json
{
  "session_id": "sess_...",
  "resource_uri": "singleton-session:/sess_...",
  "status": "idle",
  "backend": {
    "backend_id": "copilot",
    "backend_session_id": "opaque-provider-id"
  },
  "workspace": {
    "workspace_id": "work_...",
    "path": "/path/to/worktree",
    "branch": "topic-branch"
  },
  "active_turn": null,
  "latest_event_cursor": "44",
  "pending_requests": []
}
```

### 4.9 `resolve_request`

Purpose: resolve permission, input, or elicitation requests.

Input:

```json
{
  "request_id": "req_...",
  "decision": "approve",
  "response": {
    "scope": "once"
  },
  "reason": null
}
```

Supported decisions:

- `approve`
- `deny`
- `respond`
- `cancel`

Output:

```json
{
  "resolved": true,
  "request_id": "req_...",
  "status": "resolved"
}
```

### 4.10 `cancel_turn`

Purpose: cancel a running turn.

Input:

```json
{
  "session_id": "sess_...",
  "turn_id": "turn_..."
}
```

If `turn_id` is omitted, the active turn for the session is cancelled.

Output:

```json
{
  "cancelled": true,
  "turn_id": "turn_..."
}
```

### 4.11 `close_resource`

Purpose: archive, dispose, or delete sessions and workspaces.

Input:

```json
{
  "target": {
    "workspace_id": "work_..."
  },
  "disposition": "delete",
  "force": false
}
```

Supported dispositions:

- `archive`
- `dispose`
- `delete`

Output:

```json
{
  "closed": true,
  "target_uri": "singleton-workspace:/work_...",
  "cleanup_summary": {
    "deleted_paths": ["/path/to/worktree"],
    "skipped": []
  }
}
```

Rules:

- closing a session never implicitly deletes a workspace unless the workspace
  cleanup policy permits it
- deleting a workspace with active sessions fails unless `force=true`
- repeated calls are safe and idempotent

---

## 5. Persistent Store Interfaces

The store should expose repositories rather than raw SQL from business logic.

Required repository capabilities:

- allocate stable ids and resource URIs
- insert singleton intents before backend calls
- append events with monotonically increasing `server_seq`
- read events by resource and cursor
- create and update hosts/workspaces/sessions/chats/turns/requests
- resolve requests atomically
- mark sessions degraded when backend resume fails
- archive/dispose/delete resources with idempotent semantics

Large payloads may be stored in JSONL/blob files referenced from SQLite, but
the event row must retain enough metadata for filtering and cursoring.

---

## 6. Event Types

Event type names should be stable, dot-separated, and resource-oriented.

Initial categories:

- `host.available`
- `host.unavailable`
- `workspace.created`
- `workspace.ready`
- `workspace.closed`
- `workspace.changeset.created`
- `session.created`
- `session.resumed`
- `session.status_changed`
- `session.archived`
- `turn.queued`
- `turn.started`
- `turn.completed`
- `turn.failed`
- `turn.cancelled`
- `message.delta`
- `message.completed`
- `request.created`
- `request.resolved`
- `backend.event`
- `backend.error`

Backend-specific payloads must stay in `payload_json`; do not leak
provider-native ids into public ids except inside explicit backend metadata.

---

## 7. CLI Interface

The CLI is secondary and should be thin.

Initial commands:

```bash
singleton serve
singleton status
singleton stop
```

Optional later commands:

```bash
singleton attach <session_id>
singleton export <resource_uri>
singleton doctor
```

The CLI must call the same broker APIs as MCP tools whenever possible.

---

## 8. Compatibility and Migration

The previous Python daemon, hub, worker, hook, and TUI interfaces are
historical reference only. New work should not extend those contracts except to
mine tests, examples, or behavior notes during the Rust rewrite.

The repository may temporarily contain both Python and Rust code while the
replacement lands. During that period, docs must clearly identify which
interfaces are current and which are superseded.
