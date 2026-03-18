# singleton — Behavioral Specification

## 1. Overview

`singleton` is a CLI tool that provides a single, persistent hub conversation (the "hub session") through which a user manages multiple background agent threads ("worker sessions"). The user never directly creates or attaches to worker sessions; all interaction goes through the hub. The hub agent reacts autonomously to worker events without requiring user intervention, except when a `passthrough` permission request explicitly demands it.

---

## 2. System Components

### 2.1 Daemon

The `singleton` daemon is a long-running background process that acts as the central broker. It:

- Creates and owns the hub session's pseudoterminal (pty)
- Spawns, pipes, and monitors all worker processes
- Watches `~/.singleton/threads/*/events/` for worker-emitted events using an asyncio file watcher (`watchfiles`)
- Injects messages into the hub's pty input when worker events occur
- Serves the MCP HTTP server that the hub uses to interact with workers
- Exposes a Unix socket (`~/.singleton/daemon.sock`) for the `singleton` CLI to connect to

The daemon is started automatically by `singleton` if not already running. It persists until `singleton stop` is called.

### 2.2 Hub Session

The hub is a standard `claude` interactive session running inside a pty owned by the daemon. It has full Claude Code TUI capabilities (markdown rendering, tool approval UI, etc.) because it runs as a real `claude` process — not a stream-json subprocess.

The hub runs from a dedicated directory (`~/.singleton/hub/`) with two config files written by the daemon before spawn:

- **`~/.singleton/hub/.mcp.json`** — MCP server configuration:
  ```json
  {"mcpServers": {"singleton": {"type": "http", "url": "http://127.0.0.1:32100/mcp"}}}
  ```
- **`~/.singleton/hub/.claude/settings.json`** — enables the MCP server and pre-approves all singleton tools:
  ```json
  {
    "enabledMcpjsonServers": ["singleton"],
    "permissions": {
      "allow": ["mcp__singleton__create_thread", "mcp__singleton__list_threads", ...]
    }
  }
  ```

The hub is spawned as `claude` (no extra flags) with `cwd=~/.singleton/hub/`.

> **Note**: `mcpServers` is **not** supported in `settings.json` at any scope — it must be in `.mcp.json` (see https://code.claude.com/docs/en/settings#what-uses-scopes). Passing `mcpServers` via `--settings` works only in `--print` mode and silently does nothing in interactive sessions.

The `allowedTools` list grants autonomous access to exactly the 11 singleton MCP tools the hub needs to manage workers and handle approvals. All other tools (Bash, Edit, Write, etc.) remain subject to normal Claude Code permission prompts shown to the user.

The hub's stdin and stdout are the master pty fd. The `singleton` CLI relays between this fd and the user's terminal.

The hub persists across CLI detach/re-attach cycles. Its session ID is stored in `~/.singleton/hub_session_id` so it can be resumed after a daemon restart.

### 2.3 Worker Sessions

Workers are `claude --print --input-format=stream-json --output-format=stream-json` processes. The daemon holds their stdin/stdout pipes. Workers are long-lived: their stdin is kept open (no EOF sent) so the process stays alive between turns.

Each worker has:
- A thread ID (short unique identifier, e.g. `abc123`)
- A working directory (CWD), specified at creation or defaulting to `~/.singleton/workers/default/`
- A permissions mode: `yolo`, `supervised` (default), or `passthrough`
- Per-thread hooks injected via `--settings` on the CLI; does not touch the project's `.claude/settings.json`
- A system prompt appended via `--append-system-prompt` identifying it as thread `{thread_id}`

### 2.4 MCP Server

The daemon embeds an HTTP MCP server (FastMCP, HTTP/SSE transport) on `localhost:32100` (configurable). The hub connects to it at startup. MCP tools are the only way the hub interacts with workers — it never directly manages processes.

### 2.5 `singleton` CLI

The CLI is the user-facing entry point. It connects to the daemon's Unix socket and relays terminal I/O between the user and the hub's pty.

---

## 3. CLI Behavior

### Commands

| Command | Behavior |
|---|---|
| `singleton` | If daemon running: attach to hub pty. If not: start daemon + attach. |
| `singleton attach` | Attach to existing hub pty (same as bare `singleton` when daemon is running). |
| `singleton status` | Print thread status board to stdout without attaching. |
| `singleton stop` | Gracefully stop daemon, hub, and all workers. |

### Terminal relay

When attached, the CLI operates in raw terminal mode. All bytes from the user's keyboard are forwarded to the hub pty input, and all bytes from the hub pty output are forwarded to the user's terminal — except the prefix sequence.

**Prefix key**: default `Ctrl+b` (byte `\x02`). When the CLI receives the prefix byte, it enters command mode for the next keystroke:
- `d` — detach (CLI exits, hub and daemon continue running)
- `?` — print available prefix commands to the relay
- Any other byte — forward both the prefix byte and the command byte as-is

### Multi-attach

Multiple `singleton attach` instances can connect simultaneously. The daemon fans hub pty output to all attached CLI connections. Input from any attached CLI is forwarded to the hub pty.

---

## 4. Worker Lifecycle

### Creation

`create_thread(description, context="", cwd=None, permissions_mode="supervised")` creates a thread and spawns the worker. Returns `{thread_id}`.

- If `cwd` is `None`, the worker's CWD is `~/.singleton/workers/default/`
- `~/.singleton/workers/default/` may contain a `.claude/settings.json` with default worker configuration (model, tools, system prompt additions); this is respected by the worker normally
- Per-thread hooks are injected via `--settings <json>` at spawn time

### Status transitions

```
pending → running → idle ↔ running
                 → awaiting_approval → running
                 → cancelled
                 → done (terminal)
```

- `running`: worker is actively processing a turn (model is generating)
- `idle`: worker has completed a turn and is waiting for the next message
- `awaiting_approval`: worker's `PreToolUse` hook has paused execution, waiting for approval
- `done`: worker's process has exited (Stop hook fired, no more turns expected)
- `cancelled`: hub called `cancel_thread`, worker was SIGTERM'd

### Sending a message to a worker

`send_to_thread(thread_id, message)` writes a stream-json user turn to the worker's stdin pipe and blocks until the worker emits a `result` event on stdout. Returns `{result_text}` (the worker's final assistant text for that turn).

After `send_to_thread` returns, the daemon also injects a summary into the hub pty (see §5).

### Cancellation

`cancel_thread(thread_id)` sends SIGTERM to the worker process. Worker status transitions to `cancelled`.

---

## 5. Worker Output → Hub (Layered Visibility)

### Default injection (auto, on idle)

When a worker turn completes (daemon reads a `result` event from the worker's stdout stream-json), the daemon:
1. Extracts assistant text content from the turn's `assistant` message events (ignores `tool_use` blocks)
2. Truncates to ≤500 characters
3. Injects into the hub pty input as a formatted message:

```
[TASK abc123 — idle] "Description here"
Result: <truncated assistant text>
Use thread_output("abc123") or send_to_thread("abc123", ...) for details.
```

This injection respects the hub_busy coordination (see §7).

### On-demand inspection (hub-initiated)

The hub can request full traces via MCP:

- `thread_output(thread_id, page=0, page_size=50)` — returns paginated lines from `output.txt`. `page=0` = most recent `page_size` lines; incrementing `page` walks backwards. Returns `{lines, total_lines, has_more}`.
- `get_thread_events(thread_id, page=0, page_size=10)` — returns paginated structured events (tool calls, errors, completions, approval requests). Same pagination semantics. Returns `{events, total, has_more}`.

---

## 6. Permissions Framework

### `yolo` mode

Worker spawned with `--dangerously-skip-permissions`. No hook intervention. Worker runs fully autonomously.

### `supervised` mode (default)

Worker spawned without `--dangerously-skip-permissions`. A `PreToolUse` hook fires on every tool call:

1. Hook writes `{request_id, thread_id, tool, input, created_at}` to `~/.singleton/threads/{id}/pending/{req_id}.json`
2. Hook writes a signal event to `~/.singleton/threads/{id}/events/{event_id}.json`
3. Daemon's file watcher detects the event and injects into hub pty:
   ```
   [TASK abc123 — awaiting approval] Bash('rm -rf /tmp/foo')
   Call approve_tool_call("req_1") or deny_tool_call("req_1")
   ```
4. Hub calls `approve_tool_call(req_id)` or `deny_tool_call(req_id)` via MCP
5. Daemon writes response to `~/.singleton/threads/{id}/responses/{req_id}.json`
6. Hook reads response and exits `0` (allow) or `2` (block)

Timeout: hook polls every 1 second, up to 300 iterations (5 minutes). On timeout, exits `2` (safe default: block).

### `passthrough` mode

Worker spawned without `--dangerously-skip-permissions`. `PreToolUse` hook fires:

1–2. Same as supervised (write pending file + event)
3. Daemon detects event; instead of injecting into hub, daemon temporarily suspends the pty relay and writes a direct prompt to the user's terminal:
   ```
   [TASK abc123] Bash('rm -rf /tmp/foo')
   Approve? [a/d]:
   ```
4. User types `a` (approve) or `d` (deny); daemon writes response file and resumes pty relay
5. Hook unblocks as in supervised

`passthrough` is appropriate for high-stakes threads where the user wants direct control, bypassing hub agent judgment.

### Dynamic mode change

`set_thread_permissions(thread_id, mode)` changes the mode stored in `thread.json`. Takes effect on the next worker turn (hook script reads mode from `thread.json` at each invocation).

---

## 7. Hub Injection Coordination

The daemon tracks `hub_busy` state:
- Set to `True` when bytes appear on the hub pty output (hub is generating)
- Set to `False` when hub pty output goes quiet for >200ms

Injections are queued when `hub_busy=True`. The queue holds up to 10 pending injections. When hub becomes idle, queued injections fire in order.

For `passthrough` approvals, the daemon suspends the pty relay regardless of `hub_busy` (passthrough is time-sensitive).

---

## 8. State Layout

```
~/.singleton/
  daemon.pid             # daemon process ID
  daemon.sock            # unix socket: CLI ↔ daemon
  mcp.port               # daemon's MCP HTTP port (default: 32100)
  hub_session_id         # hub session ID for crash recovery
  logs/
    {YYYYMMDDTHHMMSS}_{pid}.daemon.log   # one log file per daemon run
  hub/                   # hub working directory (written by daemon before spawn)
    .mcp.json            # MCP server config for the hub session
    .claude/
      settings.json      # allowedTools for the hub session
  workers/
    default/             # default worker CWD; user places .claude/settings.json here
  threads/
    {thread_id}/
      thread.json          # {id, description, context, cwd, status, permissions_mode, pid, created_at, updated_at}
      output.txt         # all worker stdout (all turns, appended)
      events/            # hook-written event files; daemon watches this dir
        {event_id}.json  # {event_id, thread_id, type, data, timestamp}
      pending/           # PreToolUse approval requests (supervised + passthrough)
        {req_id}.json    # {request_id, thread_id, tool, input, created_at}
      responses/         # hub's or user's approve/deny decisions
        {req_id}.json    # {request_id, decision: "approve"|"deny", decided_at}
```

### Daemon logs

Each daemon run writes all log output to a dedicated file: `~/.singleton/logs/{YYYYMMDDTHHMMSS}_{pid}.daemon.log`. The timestamp uses local time in compact ISO-8601 format (`20260308T132731`). Log files accumulate across runs and are not automatically rotated or deleted — users may prune `~/.singleton/logs/` freely. The log path is printed to stdout when the daemon starts.

---

## 9. Crash Recovery

On daemon restart:
1. Reads `thread.json` for each thread; checks if `pid` is still alive via `os.kill(pid, 0)`
2. For alive workers: attempts to re-open their stdin/stdout pipes. If re-opening is not possible (pipes are owned by the old daemon process), marks status as `disconnected`.
3. For dead workers: marks status as `done` if last known status was `running` or `idle`; leaves `cancelled`/`done` unchanged
4. Reads `hub_session_id` and starts a new hub with `--resume {hub_session_id}` to continue the conversation

---

## 10. MCP Tool Reference

### Thread lifecycle

| Tool | Signature | Description |
|---|---|---|
| `create_thread` | `(description, context="", cwd=None, permissions_mode="supervised")` → `{thread_id}` | Create thread and spawn worker |
| `list_threads` | `()` → `[{id, description, cwd, status, permissions_mode, created_at}]` | List all threads |
| `get_thread` | `(thread_id)` → `{metadata, last_turn_summary}` | Thread metadata + last turn summary |
| `thread_output` | `(thread_id, page=0, page_size=50)` → `{lines, total_lines, has_more}` | Paginated output; page=0=latest |
| `get_thread_events` | `(thread_id, page=0, page_size=10)` → `{events, total, has_more}` | Paginated events; page=0=latest |
| `send_to_thread` | `(thread_id, message)` → `{result_text}` | Send turn; blocks until complete |
| `cancel_thread` | `(thread_id)` → `bool` | SIGTERM worker |
| `set_thread_permissions` | `(thread_id, mode)` → `bool` | Change permission mode |

### Approval management

| Tool | Signature | Description |
|---|---|---|
| `list_pending_approvals` | `()` → `[{request_id, thread_id, tool, input, created_at}]` | All pending approvals |
| `approve_tool_call` | `(request_id)` → `bool` | Allow the tool call |
| `deny_tool_call` | `(request_id)` → `bool` | Block the tool call |

---

## 11. Skills

| Skill | Trigger | Behavior |
|---|---|---|
| `/new-thread` | User invokes `/new-thread` | Hub gathers description, optional cwd, permissions mode (default: supervised); calls `create_thread` |
| `/threads` | User invokes `/threads` | Hub calls `list_threads` + `list_pending_approvals`; renders status board with actionable items highlighted |
| `/focus` | User invokes `/focus` | Hub asks for thread ID; calls `get_thread` + `thread_output(page=0)`; sets conversational context for continued work |

---

## 12. Default Worker Configuration

`~/.singleton/workers/default/` is the default CWD for workers without an explicit `cwd`. Users may place a `.claude/settings.json` in this directory to configure:
- Default model
- Default allowed/disallowed tools
- System prompt additions
- Any other Claude Code session settings

This configuration is loaded by the worker process normally, as if the user had configured a project directory.

---

## 13. Interface Specifications

See `spec/interfaces.md` for complete interface contracts: Unix socket protocol (CLI ↔ daemon), MCP tool signatures and return types, stream-json worker protocol, hook script environment/stdin/exit codes, all state file JSON schemas, hub injection text formats, and hub/worker spawn commands.

---

## 14. Non-Goals (Current Scope)

- Web UI or GUI
- Multi-user / networked access
- Worker-to-worker communication (workers communicate only through the hub)
- Automatic thread scheduling or triggers
- Integration with specific version control systems (hub can instruct workers to use git, but singleton has no git awareness)
