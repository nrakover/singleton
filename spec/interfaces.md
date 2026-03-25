# singleton - Interface Specifications

This document defines the current cross-component contracts for the rewritten architecture: daemon/client socket messages, MCP tools, SQLite schema, hook contracts, worker spawn rules, and TUI ownership behavior.

---

## Interface Map

```
User terminal -> singleton CLI/TUI client
                        |
                 daemon.sock (unix socket)
                        |
                      Daemon
                 +------+--------+
                 |               |
          Hub controller      Worker session manager
          (in-memory)         (spawns one run/process)
                 |               |
          claude -p hub     claude -p worker runs
                                 |
                    direct Python hook commands
                                 |
                           SQLite messages.db
                                 |
                          JSONL stdout/stderr logs
```

---

## I-1: CLI <-> Daemon Unix Socket Protocol

Socket path: `~/.singleton/daemon.sock`

All socket messages are newline-delimited JSON.

### CLI -> Daemon

#### `attach`
```json
{"type": "attach", "tty_rows": 40, "tty_cols": 120}
```

#### `detach`
```json
{"type": "detach"}
```

#### `input`
```json
{"type": "input", "data": "<base64-encoded bytes>"}
```

#### `resize`
```json
{"type": "resize", "tty_rows": 45, "tty_cols": 130}
```

#### `status_request`
```json
{"type": "status_request"}
```

#### `take_control`
```json
{"type": "take_control"}
```

#### `passthrough_response`
```json
{"type": "passthrough_response", "request_id": "req_abc", "decision": "deny", "reason": "Too risky"}
```

### Daemon -> CLI

#### `render`
```json
{"type": "render", "view_model": {...}}
```

#### `status_response`
```json
{"type": "status_response", "threads": [...], "pending_approvals": [...]} 
```

#### `control_granted`
```json
{"type": "control_granted"}
```

#### `control_denied`
```json
{"type": "control_denied", "owner": "client_2"}
```

#### `passthrough_prompt`
```json
{"type": "passthrough_prompt", "request_id": "req_abc", "thread_id": "t1", "tool": "Bash", "input": {"command": "..."}}
```

#### `ack`
```json
{"type": "ack"}
```

#### `error`
```json
{"type": "error", "message": "..."}
```

---

## I-2: MCP Tool Interface

Transport: FastMCP HTTP on `http://127.0.0.1:{port}/mcp`

Hub connects by running from `~/.singleton/hub/`, which contains `.mcp.json`. `mcpServers` must not be placed in `settings.json`.

Tools:

- `create_thread(description, context="", cwd=None, permissions_mode="supervised") -> {thread_id}`
- `list_threads() -> [...]`
- `get_thread(thread_id) -> {...}`
- `thread_output(thread_id, page=0, page_size=50) -> {...}`
- `get_thread_events(thread_id, page=0, page_size=10) -> {...}`
- `send_to_thread(thread_id, message) -> {result_text, outcome}`
- `cancel_thread(thread_id) -> {cancelled: bool}`
- `set_thread_permissions(thread_id, mode) -> {updated: bool, mode: str}`
- `list_pending_approvals() -> [...]`
- `approve_tool_call(request_id) -> {approved: bool}`
- `deny_tool_call(request_id, reason="") -> {denied: bool}`

`send_to_thread` creates a new run and waits for the terminal `run_finished` event associated with that run.

---

## I-3: SQLite Schema

Database path: `~/.singleton/messages.db`

### `threads`
```sql
CREATE TABLE threads (
  thread_id TEXT PRIMARY KEY,
  description TEXT NOT NULL,
  context TEXT NOT NULL,
  cwd TEXT NOT NULL,
  permissions_mode TEXT NOT NULL,
  session_id TEXT,
  created_at TEXT NOT NULL,
  updated_at TEXT NOT NULL
);
```

### `runs`
```sql
CREATE TABLE runs (
  run_id TEXT PRIMARY KEY,
  thread_id TEXT NOT NULL REFERENCES threads(thread_id),
  created_at TEXT NOT NULL,
  pid INTEGER,
  finished_at TEXT,
  exit_code INTEGER
);
```

### `messages`
```sql
CREATE TABLE messages (
  message_id TEXT PRIMARY KEY,
  direction TEXT NOT NULL,
  message_type TEXT NOT NULL,
  thread_id TEXT NOT NULL REFERENCES threads(thread_id),
  run_id TEXT NOT NULL REFERENCES runs(run_id),
  payload_json TEXT NOT NULL,
  created_at TEXT NOT NULL
);
```

`direction` values:
- `to_worker`
- `from_worker`

`message_type` values:
- `run_started`
- `permission_request`
- `permission_resolution`
- `run_finished`
- `notification`

SQLite usage rules:
- prefer inserts over updates
- only sparse metadata is updated in `threads` and `runs`
- no durable daemon-originated request queue exists in v1

---

## I-4: Durable Message Payloads

### `run_started`
Direction: `from_worker`
```json
{
  "session_id": "session-uuid",
  "source": "startup"
}
```

### `permission_request`
Direction: `from_worker`
```json
{
  "request_id": "req_123",
  "tool_name": "Bash",
  "tool_input": {"command": "rm -rf build"},
  "permission_mode": "supervised",
  "permission_suggestions": []
}
```

### `permission_resolution`
Direction: `to_worker`
```json
{
  "request_id": "req_123",
  "decision": "deny",
  "resolved_by": "hub",
  "reason": "Need explicit review first"
}
```

### `run_finished`
Direction: `from_worker`
```json
{
  "outcome": "completed",
  "session_id": "session-uuid",
  "result_text": "Updated the API client and tests.",
  "error": null,
  "error_details": null
}
```

Failure example:
```json
{
  "outcome": "api_error",
  "session_id": "session-uuid",
  "result_text": "API Error: Rate limit reached",
  "error": "rate_limit",
  "error_details": "429 Too Many Requests"
}
```

### `notification`
Direction: `from_worker`
```json
{"text": "Claude emitted a notification."}
```

---

## I-5: Hook Contract

Hooks are command hooks implemented as direct Python entrypoints.

### Shared environment

| Variable | Description |
|---|---|
| `SINGLETON_THREAD_ID` | Durable parent thread |
| `SINGLETON_RUN_ID` | Current run id |
| `SINGLETON_DB_PATH` | Absolute path to `messages.db` |
| `SINGLETON_STATE_DIR` | Absolute path to `~/.singleton/` |

### `SessionStart`
- Trigger: worker session startup or resume
- Responsibility: append `run_started`
- Uses hook stdin `session_id` and `source`
- Exit: `0`

### `PermissionRequest`
- Trigger: Claude is about to show a permission dialog
- Responsibility:
  1. append `permission_request`
  2. poll SQLite for a matching `permission_resolution`
  3. return Claude-compatible allow/deny JSON
- Deny responses may include a freeform `reason`

### `Stop`
- Trigger: Claude completed the turn successfully
- Responsibility: append `run_finished` with `outcome="completed"`
- Uses `session_id` and `last_assistant_message`

### `StopFailure`
- Trigger: Claude ended due to API error
- Responsibility: append `run_finished` with `outcome="api_error"`
- Uses `session_id`, `error`, `error_details`, and `last_assistant_message`

### `Notification`
- Optional; append `notification`

---

## I-6: Worker Spawn Interface

Each worker run is a fresh subprocess.

```python
cmd = [
    "claude",
    "-p",
    "--output-format=stream-json",
    "--verbose",
    "--settings", settings_json,
    *( ["--resume", session_id] if session_id else [] ),
    prompt_text,
]
```

Notes:
- exact Claude flags may be refined during implementation, but the process model is one request per subprocess
- `run_id` must exist before spawn
- hook environment must include `SINGLETON_THREAD_ID` and `SINGLETON_RUN_ID`
- stdout and stderr are streamed to `{run_id}.stdout.jsonl` and `{run_id}.stderr.jsonl`

---

## I-7: Hub Spawn Interface

The hub is daemon-owned and long-lived.

```python
cmd = [
    "claude",
    "-p",
    "--output-format=stream-json",
    "--verbose",
]
```

The daemon writes prompts to hub stdin and consumes hub stdout in memory, then renders them via the singleton TUI.

The hub working directory contains:
- `.mcp.json` with the singleton MCP server
- `.claude/settings.json` enabling that MCP server and pre-allowing singleton MCP tools

---

## I-8: TUI Ownership Rules

- There is one canonical daemon-owned app state
- Each attached client has its own renderer state, including terminal dimensions
- Exactly one client owns freeform hub input
- Non-owning clients may observe all rendered state but may not type into the hub until they take control
- Passthrough approvals route to the current owner first

---

## Interface Dependency Summary

| Interface | Producer | Consumer |
|---|---|---|
| I-1 Unix socket protocol | CLI, Daemon | CLI, Daemon |
| I-2 MCP tools | Daemon | Hub |
| I-3 SQLite schema | Daemon, hooks | Daemon, hooks, MCP layer |
| I-4 Durable messages | Hooks, daemon | Daemon, hooks, MCP layer |
| I-5 Hook contract | Claude Code, daemon | Python hook entrypoints |
| I-6 Worker spawn | Worker session manager | Claude CLI |
| I-7 Hub spawn | Hub controller | Claude CLI |
| I-8 TUI ownership | Daemon | Attached clients |
