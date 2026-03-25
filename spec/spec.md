# singleton - Behavioral Specification

## 1. Overview

`singleton` is a CLI tool that provides one daemon-owned hub conversation through which a user manages multiple background worker threads. The user never directly attaches to worker sessions. The hub remains the control surface, while worker runs are durable, one-request-per-process Claude Code sessions that can outlive the daemon.

The architecture is intentionally split into two planes:

- an ephemeral hub plane owned by the daemon
- a durable worker plane backed by SQLite messages and JSONL session logs

---

## 2. System Components

### 2.1 Daemon

The `singleton` daemon is the singleton control process. It:

- owns the long-lived hub subprocess
- serves the MCP server used by the hub
- manages attached CLI/TUI clients
- maintains canonical runtime UI state in memory
- launches worker runs on demand
- rebuilds runtime state from durable worker data after restart

The daemon is started automatically by `singleton` if not already running. It persists until `singleton stop` is called or the process exits unexpectedly.

### 2.2 Hub Session

The hub is a daemon-owned long-lived `claude -p` process with streaming input and streaming output. It runs from `~/.singleton/hub/` so Claude Code discovers `.mcp.json` naturally.

The daemon communicates with the hub in memory only. There is no durable queue between daemon and hub. If the daemon dies, the hub dies too.

The singleton TUI, not Claude's native TUI, is responsible for rendering the hub session and surrounding application state.

### 2.3 Worker Threads and Runs

A worker thread is a durable logical conversation. A worker run is one concrete subprocess invocation handling a single request for that thread.

Each thread has:
- a stable `thread_id`
- a description and optional context
- a working directory
- a permissions mode: `yolo`, `supervised`, or `passthrough`
- a nullable `session_id` used to resume subsequent runs

Each run has:
- a stable `run_id`
- a parent `thread_id`
- one spawned `claude -p` subprocess
- full-fidelity stdout/stderr logs written to JSONL files

Workers are never kept alive idly between requests. Follow-up requests spawn a new process with `--resume <session_id>` when available.

### 2.4 SQLite Worker Plane

Worker-originated lifecycle facts are written into a dedicated SQLite database, `~/.singleton/messages.db`.

SQLite stores:
- thread metadata
- run metadata
- durable messages emitted by hooks and by daemon permission resolutions

SQLite does not store full stream logs. Raw stdout/stderr event streams are written to JSONL files.

### 2.5 Hooks

Worker hooks are direct Python command hooks. They do not invoke bash wrapper scripts.

The authoritative worker lifecycle hooks are:
- `SessionStart`
- `PermissionRequest`
- `Stop`
- `StopFailure`
- optionally `Notification`

Hooks receive `SINGLETON_THREAD_ID`, `SINGLETON_RUN_ID`, and the SQLite location in their environment.

### 2.6 `singleton` CLI / TUI

The CLI is the user-facing entry point. It connects to the daemon over a Unix socket. Attached clients render a singleton-owned TUI, not a raw PTY relay.

Multiple clients may attach simultaneously. All see mirrored application state, but only one client owns freeform hub input at a time.

---

## 3. CLI Behavior

### Commands

| Command | Behavior |
|---|---|
| `singleton` | If daemon running: attach to the singleton TUI. If not: start daemon + attach. |
| `singleton attach` | Attach another client to the running singleton TUI. |
| `singleton status` | Print thread and approval status without attaching. |
| `singleton stop` | Gracefully stop daemon and hub. Running workers may continue only if not explicitly terminated by shutdown policy. |

### Prefix key

The default prefix key remains `Ctrl+b`.

- `d` - detach this client
- `?` - show prefix help
- other keys - forwarded according to the active input mode

### Multi-attach

The daemon supports multiple attached clients.

- all clients receive mirrored rendered output
- exactly one client is the active hub input owner
- non-owning clients are read-only until they explicitly take control
- passthrough approval prompts target the active input owner first; if none exists, the daemon may designate a temporary owner

---

## 4. Worker Lifecycle

### Thread creation

`create_thread(description, context="", cwd=None, permissions_mode="supervised")` creates a durable thread record and returns `{thread_id}`. It does not require a permanently running worker subprocess.

If `cwd` is `None`, the worker CWD defaults to `~/.singleton/workers/default/`.

### Starting a run

When the hub asks a thread to do work, the daemon:
1. creates a `run` record first
2. spawns a worker subprocess for that run
3. injects hook environment including `SINGLETON_THREAD_ID` and `SINGLETON_RUN_ID`
4. resumes with `--resume <session_id>` when the thread already has one

### Lifecycle facts

Run lifecycle is derived from durable messages plus sparse metadata:

- `run_started` from `SessionStart`
- `permission_request` from `PermissionRequest`
- `permission_resolution` from hub or user decision flow
- `run_finished` from `Stop` or `StopFailure`

Subprocess exit observation is fallback recovery for abnormal termination not covered by hooks.

### Sending a message to a worker

`send_to_thread(thread_id, message)` creates a new run for the thread, waits for that run to finish, and returns the terminal summary extracted from the authoritative completion event.

---

## 5. Worker Output and Inspection

### Durable logs

Each worker run writes full stdout/stderr stream output to JSONL log files under the thread directory. These logs are append-only and are not authored by hooks.

### Default visibility

When a worker run finishes or requests permission, the daemon reflects that durable event into the hub/TUI state. The hub sees summarized worker outcomes rather than raw full logs by default.

### On-demand inspection

The hub can inspect:

- recent run logs via `thread_output(...)`
- recent durable worker events via `get_thread_events(...)`

Pagination walks backward from the newest data.

---

## 6. Permissions Framework

### `yolo`

Workers run with Claude-native bypass permissions. No approval hooks are expected to block execution.

### `supervised`

Workers run with normal Claude permissions. When Claude is about to show a permission dialog, the `PermissionRequest` hook:

1. writes a durable `permission_request` message to SQLite
2. waits for a matching `permission_resolution`
3. returns an allow/deny decision back to Claude, including an optional deny reason

The hub is expected to handle these requests autonomously when appropriate.

### `passthrough`

The `PermissionRequest` hook writes the same durable request message, but the daemon routes the request to the active attached user instead of expecting the hub to decide.

### Dynamic mode change

`set_thread_permissions(thread_id, mode)` updates the durable thread metadata. The new mode applies on the next run.

---

## 7. State Layout

```
~/.singleton/
  daemon.pid
  daemon.sock
  mcp.port
  messages.db
  logs/
    {YYYYMMDDTHHMMSS}_{pid}.daemon.log
  hub/
    .mcp.json
    .claude/
      settings.json
  workers/
    default/
  threads/
    {thread_id}/
      runs/
        {run_id}.stdout.jsonl
        {run_id}.stderr.jsonl
```

SQLite holds durable thread/run/message records. JSONL holds full-fidelity process streams.

---

## 8. Crash Recovery

If the daemon crashes:

- the hub is lost and will be restarted fresh with the daemon
- attached clients disconnect
- worker subprocesses may continue running
- worker hooks continue writing durable messages into SQLite

On daemon restart:

1. the daemon reads SQLite durable state
2. rebuilds unresolved permission requests and recent run lifecycle state
3. reconciles unfinished runs with observed subprocess state and terminal messages
4. starts a fresh hub process
5. rebuilds the TUI state for new attachments

---

## 9. MCP Tool Reference

### Thread lifecycle

| Tool | Signature | Description |
|---|---|---|
| `create_thread` | `(description, context="", cwd=None, permissions_mode="supervised")` -> `{thread_id}` | Create durable thread metadata |
| `list_threads` | `()` -> `[{id, description, cwd, permissions_mode, created_at, derived_status}]` | List all threads |
| `get_thread` | `(thread_id)` -> `{metadata, last_run_summary}` | Thread metadata + latest summary |
| `thread_output` | `(thread_id, page=0, page_size=50)` -> `{lines, total_lines, has_more}` | Paginated log output |
| `get_thread_events` | `(thread_id, page=0, page_size=10)` -> `{events, total, has_more}` | Paginated durable lifecycle events |
| `send_to_thread` | `(thread_id, message)` -> `{result_text, outcome}` | Start a new run and wait for completion |
| `cancel_thread` | `(thread_id)` -> `bool` | Cancel active run if one exists |
| `set_thread_permissions` | `(thread_id, mode)` -> `bool` | Change permission mode for future runs |

### Approval management

| Tool | Signature | Description |
|---|---|---|
| `list_pending_approvals` | `()` -> `[{request_id, thread_id, run_id, tool, input, created_at}]` | Unresolved permission requests |
| `approve_tool_call` | `(request_id)` -> `bool` | Allow the tool call |
| `deny_tool_call` | `(request_id, reason="")` -> `bool` | Block the tool call with optional reason |

---

## 10. Default Worker Configuration

`~/.singleton/workers/default/` is the default CWD for workers without an explicit `cwd`. Users may place a `.claude/settings.json` there for default worker configuration.

Per-run singleton hook configuration is still injected by the daemon and must not require mutating project settings files.

---

## 11. Interface Specifications

See `spec/interfaces.md` for the concrete SQLite schema, message payloads, hook contracts, daemon/client socket protocol, worker spawn model, and TUI ownership rules.

---

## 12. Non-Goals (Current Scope)

- Web UI or GUI
- Multi-user / networked access
- Worker-to-worker direct communication
- Durable daemon-originated command queues
- Support for multiple daemon instances consuming the same state directory
