# singleton — Interface Specifications

This document enumerates every cross-component interface in the system. Each interface specifies the contract between two components: message formats, schemas, environment variables, exit codes, and error conventions. Keep this document in sync with `spec.md`, `user_flows.md`, and `tests.md`.

---

## Interface Map

```
User terminal ──(pty bytes / socket messages)──► singleton CLI
                                                       │
                                           daemon.sock (unix socket)
                                                       │
                                                 Daemon process
                                          ┌────────────┼────────────┐
                              (pty fd)   │     (HTTP MCP)    (stdin/stdout pipes)
                                         ▼             ▼             ▼
                                     Hub process   [MCP client]  Worker processes
                                                       │
                                               (stream-json)
                                                       ▼
                                              Worker stdin/stdout
                                                       │
                                         (file writes to ~/.singleton/)
                                                       ▼
                                               Hook scripts
                                          (write events/, pending/)
                                                       │
                                          (watchfiles) ▼
                                                   Daemon
```

---

## I-1: CLI ↔ Daemon Unix Socket Protocol

Socket path: `~/.singleton/daemon.sock`

All messages are newline-delimited JSON (`\n`-terminated). The CLI opens a persistent connection per session.

### CLI → Daemon

#### `attach`
```json
{"type": "attach", "tty_rows": 40, "tty_cols": 120}
```
Registers this connection as a hub pty relay. Daemon begins forwarding hub pty output to this connection. `tty_rows`/`tty_cols` are forwarded as a `SIGWINCH` to the hub.

#### `detach`
```json
{"type": "detach"}
```
Unregisters this connection from hub pty relay. Daemon acknowledges with `{"type": "ack"}`.

#### `input`
```json
{"type": "input", "data": "<base64-encoded bytes>"}
```
Raw terminal input from user, forwarded to hub pty master fd. Base64-encoded to handle arbitrary bytes.

#### `resize`
```json
{"type": "resize", "tty_rows": 45, "tty_cols": 130}
```
Terminal resize event; daemon sends `SIGWINCH` to hub process.

#### `status_request`
```json
{"type": "status_request"}
```
Request current thread status board (used by `singleton status`).

#### `passthrough_response`
```json
{"type": "passthrough_response", "request_id": "req_abc", "decision": "approve"}
```
User's response to a passthrough approval prompt. `decision` is `"approve"` or `"deny"`.

### Daemon → CLI

#### `output`
```json
{"type": "output", "data": "<base64-encoded bytes>"}
```
Raw bytes from hub pty master fd, forwarded to user terminal.

#### `status_response`
```json
{
  "type": "status_response",
  "threads": [
    {"id": "abc123", "description": "Refactor auth", "cwd": "/repos/app",
     "status": "idle", "permissions_mode": "supervised", "created_at": "2026-03-07T10:00:00Z"}
  ],
  "pending_approvals": [
    {"request_id": "req_5f2a", "thread_id": "ghi789", "tool": "Bash",
     "input": {"command": "kubectl apply -f staging.yaml"}, "created_at": "2026-03-07T10:05:00Z"}
  ]
}
```

#### `passthrough_prompt`
```json
{
  "type": "passthrough_prompt",
  "request_id": "req_abc",
  "thread_id": "xyz789",
  "tool": "Bash",
  "input": {"command": "curl https://example.com/install.sh | bash"}
}
```
Signals CLI to suspend normal relay and show the user a direct approval prompt. CLI responds with `passthrough_response`.

#### `ack`
```json
{"type": "ack"}
```
General acknowledgment.

#### `error`
```json
{"type": "error", "message": "..."}
```

---

## I-2: MCP Tool Interface (Hub → Daemon via HTTP)

Transport: FastMCP HTTP/SSE on `http://localhost:32100/mcp`

Hub connects at startup via `--settings '{"mcpServers":{"singleton":{"type":"http","url":"http://localhost:32100/mcp"}}}'`.

All tools follow MCP tool-call conventions: named arguments, JSON return values, string error messages on failure.

### Thread lifecycle tools

#### `create_thread`
```
Input:
  description:      string        (required) Human-readable description
  context:          string        (optional, default "") Additional context for worker
  cwd:              string | null (optional, default null → ~/.singleton/workers/default/)
  permissions_mode: string        (optional, default "supervised") "yolo"|"supervised"|"passthrough"

Output:
  {"thread_id": "abc123"}

Errors:
  - "cwd does not exist: /path/to/dir"
  - "invalid permissions_mode: <value>"
```

#### `list_threads`
```
Input: (none)

Output:
  [
    {
      "id":               string,
      "description":      string,
      "cwd":              string,
      "status":           "pending"|"running"|"idle"|"awaiting_approval"|"cancelled"|"done"|"disconnected",
      "permissions_mode": "yolo"|"supervised"|"passthrough",
      "created_at":       ISO-8601 string,
      "updated_at":       ISO-8601 string
    },
    ...
  ]
  Sorted by created_at descending.
```

#### `get_thread`
```
Input:
  thread_id: string (required)

Output:
  {
    "id":               string,
    "description":      string,
    "context":          string,
    "cwd":              string,
    "status":           string,
    "permissions_mode": string,
    "pid":              integer | null,
    "created_at":       ISO-8601 string,
    "updated_at":       ISO-8601 string,
    "last_turn_summary": string  # last ≤500 chars of most recent assistant text, or "" if none
  }

Errors:
  - "thread not found: abc123"
```

#### `thread_output`
```
Input:
  thread_id: string  (required)
  page:      integer (optional, default 0)   0=latest page, 1=prior page, etc.
  page_size: integer (optional, default 50)  lines per page

Output:
  {
    "lines":       [string, ...],   # page_size lines (or fewer if at start of file)
    "total_lines": integer,
    "page":        integer,
    "has_more":    boolean          # true if page+1 exists
  }

Errors:
  - "thread not found: abc123"
```

#### `get_thread_events`
```
Input:
  thread_id: string  (required)
  page:      integer (optional, default 0)   0=latest, incrementing walks backwards
  page_size: integer (optional, default 10)

Output:
  {
    "events": [
      {
        "event_id":   string,
        "thread_id":  string,
        "type":       "stop"|"pretool"|"notification"|"turn_complete",
        "data":       object,    # type-specific, see I-5
        "timestamp":  ISO-8601 string
      },
      ...
    ],
    "total":    integer,
    "page":     integer,
    "has_more": boolean
  }

Errors:
  - "thread not found: abc123"
```

#### `send_to_thread`
```
Input:
  thread_id: string (required)
  message:   string (required)

Output:
  {
    "result_text": string,  # assistant text from completed turn (≤500 chars, truncated)
    "status":      string   # thread status after turn ("idle" or "done")
  }

Errors:
  - "thread not found: abc123"
  - "thread is not in a sendable state: <status>"  # must be idle
  - "thread is cancelled"
  - "send timeout after 300s"
```
Blocks until worker emits `result` event. Daemon serializes concurrent calls via per-thread lock.

#### `cancel_thread`
```
Input:
  thread_id: string (required)

Output:
  {"cancelled": true}

Errors:
  - "thread not found: abc123"
  - "thread already done/cancelled"
```

#### `set_thread_permissions`
```
Input:
  thread_id: string (required)
  mode:      string (required) "yolo"|"supervised"|"passthrough"

Output:
  {"updated": true, "mode": "yolo"}

Errors:
  - "thread not found: abc123"
  - "invalid mode: <value>"
```
Writes `permissions_mode` to `thread.json`. Takes effect on next hook invocation.

### Approval management tools

#### `list_pending_approvals`
```
Input: (none)

Output:
  [
    {
      "request_id": string,
      "thread_id":  string,
      "tool":       string,   # e.g. "Bash", "Edit", "Write"
      "input":      object,   # tool-specific input fields
      "created_at": ISO-8601 string
    },
    ...
  ]
  Only includes requests with no corresponding response file.
  Sorted by created_at ascending (oldest first).
```

#### `approve_tool_call`
```
Input:
  request_id: string (required)

Output:
  {"approved": true}

Errors:
  - "approval request not found: req_abc"
  - "approval already resolved: req_abc"
```

#### `deny_tool_call`
```
Input:
  request_id: string (required)

Output:
  {"denied": true}

Errors:
  - "approval request not found: req_abc"
  - "approval already resolved: req_abc"
```

---

## I-3: Worker Stream-JSON Protocol (Daemon ↔ Worker stdin/stdout)

Workers run: `claude --print --input-format=stream-json --output-format=stream-json ...`

All messages are newline-delimited JSON.

### Daemon → Worker stdin (input messages)

#### User turn
```json
{"type": "user", "message": {"role": "user", "content": "message text here"}}
```
First message after spawn carries the thread description + context as its content.

### Worker stdout → Daemon (output events)

Claude Code emits a stream of events. Daemon reads until it sees a `result` event, then considers the turn complete.

#### Assistant message
```json
{
  "type": "assistant",
  "message": {
    "id": "msg_...",
    "role": "assistant",
    "content": [
      {"type": "text", "text": "..."},
      {"type": "tool_use", "id": "...", "name": "Bash", "input": {"command": "..."}}
    ]
  }
}
```
Daemon extracts `content` blocks of `type: "text"` for turn summary. Ignores `tool_use` blocks for summary purposes.

#### Result (turn complete)
```json
{
  "type": "result",
  "subtype": "success",
  "result": "Final text output of the turn",
  "session_id": "session-uuid-here",
  "usage": {"input_tokens": 1234, "output_tokens": 567}
}
```
Signals end of turn. `result` field is the canonical final output. `session_id` is stored in `thread.json` for hub session resumption after daemon restart. On error: `"subtype": "error"`, `"error": "..."`.

#### System message (informational)
```json
{"type": "system", "subtype": "...", "message": "..."}
```
Logged to `output.txt` but not otherwise acted on.

---

## I-4: Hook Script Interface

Hooks run as subprocesses launched by the worker's Claude Code session. Each hook:
- Receives event data as JSON on **stdin**
- May write to **stdout** (for `PreToolUse`, stdout is shown to the model if exit code is non-zero)
- Exit code controls Claude Code behavior
- Must complete within reasonable time (PreToolUse is blocking)

### Environment variables (all hooks)

| Variable | Description |
|---|---|
| `SINGLETON_THREAD_ID` | Thread ID of the worker session |
| `SINGLETON_STATE_DIR` | Absolute path to `~/.singleton/` |
| `SINGLETON_HOOKS_DIR` | Absolute path to the `hooks/` directory in the repo |

### `worker-stop.sh` (Stop hook)

**Trigger**: Claude Code Stop event (worker turn complete or session ending)

**stdin**:
```json
{
  "session_id": "...",
  "stop_hook_active": true,
  "transcript": [...]
}
```

**Behavior**:
1. Writes stop event to `$SINGLETON_STATE_DIR/threads/$SINGLETON_THREAD_ID/events/{timestamp}-stop.json`
2. Updates `thread.json` `status` to `"idle"` (or `"done"` if session is ending)
3. Exits `0`

**Exit codes**: Always `0` (non-blocking).

---

### `worker-pretool.sh` (PreToolUse hook)

**Trigger**: Claude Code PreToolUse event (before any tool call)

**stdin**:
```json
{
  "tool_name": "Bash",
  "tool_input": {"command": "rm -rf /tmp/foo"},
  "session_id": "..."
}
```

**Behavior by permissions mode** (read from `$SINGLETON_STATE_DIR/threads/$SINGLETON_THREAD_ID/thread.json`):

- **`yolo`**: Exit `0` immediately. No files written.
- **`supervised`**:
  1. Generate `req_id` = `req_{timestamp}_{random4}`
  2. Write approval request to `pending/{req_id}.json` (see I-5)
  3. Write pretool event to `events/{timestamp}-pretool-{req_id}.json` (see I-5)
  4. Poll `responses/{req_id}.json` every 1 second, up to 300 iterations (5 min)
  5. On `"approve"`: exit `0`
  6. On `"deny"`: print reason to stdout; exit `2`
  7. On timeout: print "Approval timeout" to stdout; exit `2`
- **`passthrough`**: Same as `supervised` — daemon handles the routing to user terminal rather than hub.

**Exit codes**:
- `0` — allow tool call
- `2` — block tool call (stdout message shown to model as reason for blocking)

---

### `worker-notify.sh` (Notification hook)

**Trigger**: Claude Code Notification event

**stdin**:
```json
{
  "message": "...",
  "session_id": "..."
}
```

**Behavior**:
1. Writes notification event to `events/{timestamp}-notify.json` (see I-5)
2. Exits `0`

---

### Per-worker `--settings` JSON (generated by `hooks.py`)

Injected at worker spawn via `--settings '<json>'`. Merges additively with any project `.claude/settings.json`.

```json
{
  "hooks": {
    "Stop": [
      {
        "matcher": "",
        "hooks": [
          {
            "type": "command",
            "command": "SINGLETON_THREAD_ID=<id> SINGLETON_STATE_DIR=~/.singleton SINGLETON_HOOKS_DIR=<abs_path>/hooks /abs/path/hooks/worker-stop.sh"
          }
        ]
      }
    ],
    "PreToolUse": [
      {
        "matcher": "",
        "hooks": [
          {
            "type": "command",
            "command": "SINGLETON_THREAD_ID=<id> SINGLETON_STATE_DIR=~/.singleton SINGLETON_HOOKS_DIR=<abs_path>/hooks /abs/path/hooks/worker-pretool.sh"
          }
        ]
      }
    ],
    "Notification": [
      {
        "matcher": "",
        "hooks": [
          {
            "type": "command",
            "command": "SINGLETON_THREAD_ID=<id> SINGLETON_STATE_DIR=~/.singleton SINGLETON_HOOKS_DIR=<abs_path>/hooks /abs/path/hooks/worker-notify.sh"
          }
        ]
      }
    ]
  }
}
```

---

## I-5: State File Schemas

All files under `~/.singleton/`.

### `thread.json`
```json
{
  "id":               "abc123",
  "description":      "Refactor auth module",
  "context":          "",
  "cwd":              "/Users/me/repos/myapp",
  "status":           "idle",
  "permissions_mode": "supervised",
  "pid":              12345,
  "session_id":       "session-uuid",
  "created_at":       "2026-03-07T10:00:00.000Z",
  "updated_at":       "2026-03-07T10:05:00.000Z"
}
```
`status` values: `"pending"` | `"running"` | `"idle"` | `"awaiting_approval"` | `"cancelled"` | `"done"` | `"disconnected"`
`pid` is `null` when process has not yet started or has exited.
`session_id` is `null` until the first worker turn completes (extracted from stream-json `result` event).

---

### `events/{event_id}.json`

`event_id` format: `{unix_timestamp_ms}-{type}-{random4}` for natural sort order.

#### Stop event
```json
{
  "event_id":  "1741348800000-stop-a3f2",
  "thread_id": "abc123",
  "type":      "stop",
  "data":      {"session_id": "..."},
  "timestamp": "2026-03-07T10:05:00.000Z"
}
```

#### PreToolUse approval-needed event
```json
{
  "event_id":   "1741348800000-pretool-b9c1",
  "thread_id":  "abc123",
  "type":       "pretool",
  "data": {
    "request_id": "req_1741348800000_x7k2",
    "tool":       "Bash",
    "input":      {"command": "rm -rf /tmp/foo"},
    "mode":       "supervised"
  },
  "timestamp": "2026-03-07T10:05:00.000Z"
}
```

#### Notification event
```json
{
  "event_id":  "1741348800000-notify-c4e8",
  "thread_id": "abc123",
  "type":      "notification",
  "data":      {"message": "Completed file analysis"},
  "timestamp": "2026-03-07T10:05:00.000Z"
}
```

#### Turn-complete event (written by daemon, not hooks)
```json
{
  "event_id":  "1741348800000-turn-d1f9",
  "thread_id": "abc123",
  "type":      "turn_complete",
  "data": {
    "result_text":   "Added refresh token rotation, updated 3 files.",
    "session_id":    "...",
    "input_tokens":  1234,
    "output_tokens": 567
  },
  "timestamp": "2026-03-07T10:05:00.000Z"
}
```

---

### `pending/{req_id}.json`

`req_id` format: `req_{unix_timestamp_ms}_{random4}`

```json
{
  "request_id": "req_1741348800000_x7k2",
  "thread_id":  "abc123",
  "tool":       "Bash",
  "input":      {"command": "rm -rf /tmp/foo"},
  "mode":       "supervised",
  "created_at": "2026-03-07T10:05:00.000Z"
}
```

---

### `responses/{req_id}.json`

Written by daemon after hub calls `approve_tool_call` / `deny_tool_call`, or after user responds to a passthrough prompt.

```json
{
  "request_id":  "req_1741348800000_x7k2",
  "decision":    "approve",
  "decided_by":  "hub",
  "decided_at":  "2026-03-07T10:05:02.000Z"
}
```
`decided_by`: `"hub"` (MCP tool call) | `"user"` (passthrough terminal prompt)
`decision`: `"approve"` | `"deny"`

---

## I-6: Hub Injection Format

Text written directly to the hub pty master fd by the daemon. Injected messages appear as user turns in the hub conversation.

### Thread idle (after turn completes)
```
[THREAD abc123 — idle] Refactor auth module
Result: Added refresh token rotation, updated 3 files. All tests pass.
Use thread_output("abc123") or send_to_thread("abc123", ...) for details.

```
(Trailing newline submits as a user turn.)

### Approval request (supervised mode)
```
[THREAD abc123 — awaiting approval] Bash('rm -rf /tmp/foo')
Call approve_tool_call("req_1741348800000_x7k2") or deny_tool_call("req_1741348800000_x7k2")

```

### Passthrough prompt (written to CLI tty, not hub pty)
```
[THREAD abc123] Bash('curl https://example.com/install.sh | bash')
Approve? [a/d]:
```
(No trailing newline — waits for single keypress.)

---

## I-7: Hub Spawn Interface

The daemon spawns the hub process as follows:

```python
import os, subprocess, json

master_fd, slave_fd = os.openpty()

settings = {
    "mcpServers": {
        "singleton": {
            "type": "http",
            "url": f"http://localhost:{mcp_port}/mcp"
        }
    }
}

proc = await asyncio.create_subprocess_exec(
    "claude",
    "--dangerously-skip-permissions",
    "--settings", json.dumps(settings),
    *(["--resume", hub_session_id] if hub_session_id else []),
    stdin=slave_fd,
    stdout=slave_fd,
    stderr=slave_fd,
    start_new_session=True,
)
os.close(slave_fd)
# master_fd held by daemon; relayed to attached CLI connections
```

After hub exits, `hub_session_id` is read from the most recent session in `~/.claude/projects/` (or from the last `result` event captured if daemon proxied the hub's stream-json output).

---

## I-8: Worker Spawn Interface

```python
import os, json, subprocess

settings_json = hooks.generate_settings(thread_id, state_dir, hooks_dir)
system_prompt = f"You are a background worker agent for thread {thread_id}. ..."

cmd = [
    "claude", "--print",
    "--input-format=stream-json",
    "--output-format=stream-json",
    "--settings", settings_json,
    "--append-system-prompt", system_prompt,
]
if permissions_mode == "yolo":
    cmd.insert(1, "--dangerously-skip-permissions")

proc = await asyncio.create_subprocess_exec(
    *cmd,
    cwd=cwd,
    stdin=asyncio.subprocess.PIPE,
    stdout=asyncio.subprocess.PIPE,
    stderr=asyncio.subprocess.PIPE,
)
# Send initial thread description as first user turn
first_turn = json.dumps({
    "type": "user",
    "message": {"role": "user", "content": f"{description}\n\n{context}".strip()}
}) + "\n"
proc.stdin.write(first_turn.encode())
await proc.stdin.drain()
```

---

## Interface Dependency Summary

| Interface | Producer | Consumer |
|---|---|---|
| I-1: Unix socket protocol | CLI, Daemon | CLI, Daemon |
| I-2: MCP tools (HTTP) | Daemon (server) | Hub (client) |
| I-3: Stream-JSON worker protocol | Daemon (writes stdin), Worker (writes stdout) | Worker (reads stdin), Daemon (reads stdout) |
| I-4: Hook scripts | Claude Code (invokes), hooks.py (generates settings) | worker-stop.sh, worker-pretool.sh, worker-notify.sh |
| I-5: State file schemas | store.py, daemon.py, hook scripts | store.py, daemon.py, hook scripts, MCP tools |
| I-6: Hub injection format | daemon.py | Hub pty (hub session sees as user turns) |
| I-7: Hub spawn | daemon.py | claude CLI (hub) |
| I-8: Worker spawn | worker.py | claude CLI (worker) |
