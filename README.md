# singleton

A single, persistent hub conversation through which you manage multiple background Claude agent threads.

Instead of spinning up separate Claude Code sessions for each task, you maintain one hub session and let it dispatch, supervise, and coordinate background workers — all without requiring your attention unless something genuinely needs it.

## How it works

```
Your terminal
      │  pty relay
      ▼
 singleton CLI  ──── unix socket ────► singleton daemon
                                             │
                         ┌───────────────────┼───────────────────┐
                         ▼                   ▼                   ▼
                    hub process          worker A             worker B
                 (claude TUI, pty)   (stream-json)        (stream-json)
                         │
                  MCP HTTP server
               (singleton tools: create_thread,
                approve_tool_call, thread_output, ...)
```

The **daemon** is the broker: it owns all subprocess pipes, runs the MCP server the hub connects to, watches for worker events, and injects summaries and approval requests into the hub pty. The **hub** is a full interactive `claude` session. **Workers** are long-lived `claude --print` stream-json subprocesses you never talk to directly.

## Prerequisites

- macOS or Linux
- Python 3.11+
- [`uv`](https://docs.astral.sh/uv/) (`brew install uv` or `curl -LsSf https://astral.sh/uv/install.sh | sh`)
- `claude` CLI installed and authenticated (`npm install -g @anthropic-ai/claude-code` or equivalent)

## Installation

```bash
git clone https://github.com/nrakover/singleton
cd singleton
./setup.sh
```

`setup.sh` installs Python dependencies via `uv sync`, creates `~/.singleton/`, makes hook scripts executable, and writes `.claude/settings.json` pointing the hub at the local MCP server.

Then add the `singleton` binary to your PATH. With uv this is available via:

```bash
uv run singleton
# or install globally:
uv tool install .
```

## Usage

### Start (or attach to) the hub

```bash
singleton
```

If the daemon isn't running, this starts it and attaches. If it's already running, this attaches to the existing hub session.

### Prefix key: `Ctrl+b`

While attached, `Ctrl+b` is the command prefix (tmux-style):

| Sequence | Action |
|---|---|
| `Ctrl+b d` | Detach — hub and all workers keep running |
| `Ctrl+b ?` | Show prefix key help |
| `Ctrl+b <other>` | Forward `Ctrl+b` + key to hub |

### Hub skills

In the hub session, use these slash commands:

| Command | Description |
|---|---|
| `/new-thread` | Create a new background worker thread |
| `/threads` | Show thread status board + pending approvals |
| `/focus` | Load context for a specific thread |

### Other CLI commands

```bash
singleton status   # Print thread status board without attaching
singleton stop     # Gracefully stop daemon, hub, and all workers
```

## Worker permission modes

Set at thread creation (default: `supervised`):

| Mode | Behavior |
|---|---|
| `supervised` | Worker pauses at each tool call; hub approves/denies autonomously via MCP |
| `yolo` | Worker runs fully autonomously (`--dangerously-skip-permissions`) |
| `passthrough` | Worker pauses at each tool call; approval prompt goes directly to your terminal, bypassing the hub |

Change mode mid-flight: `set_thread_permissions(thread_id, "yolo")` via the hub.

## Hub session notes

**Hub tool permissions**

The hub does not run with `--dangerously-skip-permissions`. Instead, its `--settings` includes an `allowedTools` list covering exactly the 11 singleton MCP tools (`mcp__singleton__create_thread`, `mcp__singleton__approve_tool_call`, etc.). This means:

- The hub can autonomously call all singleton MCP tools with no prompts — it can manage threads, approve/deny tool calls, inspect output, etc.
- Any other tool the hub tries to use (Bash, Edit, Write, Glob, …) follows the normal Claude Code permission flow and will prompt you in the TUI.

This is the right trust boundary: the hub is trusted to coordinate workers via the singleton API, but not to run arbitrary shell commands on your machine without your knowledge. If you want the hub to also have free rein over other tools, add them to `allowedTools` in `.claude/settings.json`.

**What happens when the hub session ends?**

If you type `exit` (or the hub process otherwise terminates), your terminal restores and the CLI exits — same experience as a normal `claude` session ending. The daemon keeps running with all workers intact. Run `singleton` again to start a new hub session and pick up where you left off.

## State directory

Everything lives under `~/.singleton/`:

```
~/.singleton/
  daemon.pid           — daemon process ID
  daemon.sock          — unix socket for CLI ↔ daemon
  mcp.port             — MCP HTTP port (default: 32100)
  hub_session_id       — hub session for crash recovery resume
  workers/default/     — default worker CWD; put .claude/settings.json here
  threads/
    {thread_id}/
      thread.json      — thread metadata and status
      output.txt       — all worker stdout (append-only)
      events/          — hook-written event files
      pending/         — pending approval requests
      responses/       — approval decisions
```

Deleting `~/.singleton/` returns to a clean state (orphans any running workers, which will exit on their own).

## Development

### Setup

```bash
git clone https://github.com/nrakover/singleton
cd singleton
uv sync
```

### Run tests

```bash
uv run pytest
uv run pytest -v                     # verbose
uv run pytest tests/test_store.py    # single module
```

### Lint and type check

```bash
uv run ruff check src/ tests/
uv run ty check src/
```

### Run daemon directly (for debugging)

```bash
uv run singleton daemon --port 32100
```

Logs go to stderr. Set `PYTHONPATH` or use `uv run` to ensure the package is found.

### Project structure

```
src/singleton/
  store.py        — state r/w for ~/.singleton/ (threads, events, approvals)
  worker.py       — spawn/send_turn/cancel stream-json worker processes
  hooks.py        — generate --settings JSON for per-worker hook injection
  mcp_server.py   — FastMCP HTTP server with all 11 MCP tools
  hub.py          — Hub class: pty lifecycle, read/write, resize
  daemon.py       — asyncio coordinator: file watcher, injection queue,
                    unix socket server, crash recovery
  cli.py          — CLI entry point: attach/status/stop, raw terminal relay,
                    Ctrl+b prefix key
hooks/
  worker-stop.sh      — writes stop event on worker turn complete
  worker-pretool.sh   — writes pending approval, polls response (supervised/passthrough)
  worker-notify.sh    — writes notification event
.claude/skills/
  new-thread.md   — /new-thread skill
  threads.md      — /threads skill
  focus.md        — /focus skill
spec/             — behavioral spec, user flows, test inventory, interface contracts
tests/            — pytest unit + integration tests
```

### Adding an MCP tool

1. Add the tool function to `src/singleton/mcp_server.py` with `@mcp.tool()`.
2. Add tests to `tests/test_mcp.py`.
3. Update `spec/spec.md` §10 and `spec/interfaces.md` §I-2.

### Hook scripts

Hook scripts run inside the worker's Claude Code session. They receive event data on stdin and write files to `~/.singleton/threads/{thread_id}/`. The daemon watches for new event files via `watchfiles` and reacts immediately.

To test hook scripts manually:

```bash
export SINGLETON_THREAD_ID=test123
export SINGLETON_STATE_DIR=~/.singleton
export SINGLETON_HOOKS_DIR=$(pwd)/hooks

# Simulate a Stop hook
echo '{"session_id": "abc", "stop_hook_active": true, "transcript": []}' \
  | bash hooks/worker-stop.sh

# Simulate a PreToolUse hook (supervised mode)
echo '{"tool_name": "Bash", "tool_input": {"command": "ls"}, "session_id": "abc"}' \
  | SINGLETON_PRETOOL_TIMEOUT=3 bash hooks/worker-pretool.sh
```
