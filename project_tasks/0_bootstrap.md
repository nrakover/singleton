# Task 0: Bootstrap `singleton`

## Objective

Implement the full `singleton` system as specified in `spec/spec.md`, `spec/user_flows.md`, and `spec/tests.md`. Deliver a working `singleton` CLI that a user can install and run to get the hub+daemon+worker system operational.

Note: this bootstrap plan has been superseded architecturally by `project_tasks/1_streamed-architecture-reset.md`. Keep this file as the historical bootstrap record; current implementation work should follow the newer task plan and synchronized spec artifacts.

---

## Requirements

### Functional
- Daemon starts, manages hub pty, manages worker stream-json processes, serves MCP HTTP
- `singleton` CLI: start, attach, status, stop; `Ctrl+b d` detach
- MCP tools: all 11 tools in spec §10 implemented and tested
- Three permission modes: `yolo`, `supervised` (default), `passthrough`
- Worker output → hub injection (layered: auto-summary on idle + on-demand paginated inspection)
- Hub injection coordination: queue when hub busy, fire when quiet >200ms
- Skills: `/new-thread`, `/threads`, `/focus`
- Hook scripts: `worker-stop.sh`, `worker-pretool.sh`, `worker-notify.sh`
- Default worker CWD (`~/.singleton/workers/default/`), user-configurable
- Crash recovery: daemon restarts, re-reads thread state, resumes hub session
- `setup.sh`: installs deps, creates state dirs, writes `.claude/settings.json`

### Non-functional
- Python 3.11+, uv-managed
- UNIX only (Linux + macOS); no platform-specific APIs beyond `os.openpty()` and `os.kill()`
- No external state beyond `~/.singleton/` and process pipes
- All automated tests pass (`uv run pytest`)

---

## Assumptions

- `claude` CLI is installed and on `$PATH`
- User has a valid Anthropic session (logged into Claude Code)
- `--input-format=stream-json --output-format=stream-json` with `--print` supports multi-turn (stdin kept open) — to be validated during implementation
- FastMCP supports HTTP/SSE transport sufficient for Claude Code MCP client

## Out of scope

- Web UI or remote access
- Worker-to-worker communication
- Task scheduling / cron
- Notification system beyond pty injection

---

## Proposed Approach

### Phase A: Scaffold and store (day 1)
1. Create `pyproject.toml` with uv, entry point `singleton`
2. Implement `src/singleton/store.py` — all state r/w operations on `~/.singleton/`
3. Write `tests/test_store.py` — all T-STORE tests
4. Run tests green

### Phase B: Worker process management (day 1–2)
5. Implement `src/singleton/worker.py` — spawn, send_turn, cancel, output capture
6. Validate that `claude --print --input-format=stream-json` accepts multi-turn (stdin open) — use a simple echo mock subprocess if claude not available in test env
7. Write `tests/test_worker.py` — all T-WORKER tests
8. Run tests green

### Phase C: Hook scripts (day 2)
9. Implement `hooks/worker-stop.sh` — write stop event on Stop hook
10. Implement `hooks/worker-pretool.sh` — supervised/passthrough pending write + response polling
11. Implement `hooks/worker-notify.sh` — notification event write
12. Write `hooks.py` — generate `--settings` JSON for worker spawn
13. Add hook tests (shell-based or subprocess-invoked from pytest) — T-HOOKS

### Phase D: MCP server (day 2–3)
14. Implement `src/singleton/mcp_server.py` — FastMCP HTTP, all 11 tools
15. Write `tests/test_mcp.py` — all T-MCP tests
16. Run tests green

### Phase E: Daemon and hub pty (day 3–4)
17. Implement `src/singleton/hub.py` — pty creation, `claude` spawn in pty, relay API
18. Implement `src/singleton/daemon.py` — asyncio event loop, file watcher, hub injection coordination, passthrough overlay, Unix socket server, crash recovery
19. Write `tests/test_daemon.py` — T-DAEMON tests (using mock pty + mock workers)
20. Run tests green

### Phase F: CLI and relay (day 4)
21. Implement `src/singleton/cli.py` — start/attach/status/stop, raw terminal mode, prefix key interception, multi-attach fan-out
22. Manual T-CLI tests

### Phase G: Skills and setup (day 5)
23. Write `.claude/skills/new-thread.md`, `threads.md`, `focus.md`
24. Write `setup.sh`
25. Write `README.md`
26. Full E2E run: `./setup.sh` → `singleton` → `/new-thread` → supervised approval → completion → detach → re-attach

---

## File-Level Plan

```
pyproject.toml                         — uv config, entry point `singleton = "singleton.cli:main"`
src/singleton/__init__.py
src/singleton/store.py                 — State r/w: task CRUD, output append, events, pagination
src/singleton/worker.py                — Spawn stream-json subprocess, send_turn, cancel
src/singleton/hooks.py                 — Generate --settings JSON for worker; hook helper functions
src/singleton/mcp_server.py            — FastMCP HTTP; all MCP tools; delegates to store + worker
src/singleton/hub.py                   — os.openpty(); spawn claude in pty; expose relay API
src/singleton/daemon.py                — asyncio: file watcher, injection queue, Unix socket server, MCP start, crash recovery
src/singleton/cli.py                   — argparse: singleton / attach / status / stop; raw terminal; prefix key
hooks/worker-stop.sh                   — Write stop event to events/
hooks/worker-pretool.sh                — Write pending; poll response; exit 0/2; handle yolo auto-approve
hooks/worker-notify.sh                 — Write notification event
.claude/skills/new-thread.md
.claude/skills/threads.md
.claude/skills/focus.md
.claude/settings.json.template         — Template; setup.sh fills in absolute paths
setup.sh                               — uv sync; mkdir ~/.singleton/workers/default/; write .claude/settings.json; chmod +x
tests/__init__.py
tests/test_store.py
tests/test_worker.py
tests/test_mcp.py
tests/test_daemon.py
README.md
```

---

## Key Implementation Details

### stream-json turn format (workers)
```json
{"type": "user", "message": {"role": "user", "content": "message text here"}}
```
Sent as newline-terminated JSON to worker stdin. Worker responds with multiple events; we parse until `{"type": "result", ...}` which contains the final output.

### Worker spawn command
```bash
# supervised / passthrough:
claude --print --input-format=stream-json --output-format=stream-json \
  --settings '<hooks_json>' \
  --append-system-prompt "You are worker <thread_id>. ..." \
  [--cwd <cwd>]

# yolo:
claude --print --input-format=stream-json --output-format=stream-json --dangerously-skip-permissions \
  --settings '<hooks_json>' \
  --append-system-prompt "You are worker <thread_id>. ..." \
  [--cwd <cwd>]
```

Note: workers receive the initial thread description as the first stream-json user turn after spawn.

### Hub spawn command
```bash
claude  # no flags needed — MCP config is in .mcp.json in the hub CWD
```
Spawned with `stdin=slave_pty_fd, stdout=slave_pty_fd, stderr=slave_pty_fd, cwd=~/.singleton/hub/`.

**IMPORTANT**: `mcpServers` in `settings.json` is NOT supported (any scope; documented at https://code.claude.com/docs/en/settings#what-uses-scopes). MCP servers must be in `.mcp.json`. The hub CWD (`~/.singleton/hub/`) contains:
- `.mcp.json` — MCP server config (`{"mcpServers": {"singleton": {"type": "http", "url": "..."}}}`)
- `.claude/settings.json` — `enabledMcpjsonServers: ["singleton"]` + `permissions.allow: [...]` (enables and pre-approves singleton MCP tools)

`--settings` with `mcpServers` silently does nothing in interactive mode (works only with `--print`).

### Prefix key state machine (CLI)
```
NORMAL → [recv 0x02=Ctrl+b] → COMMAND
COMMAND → [recv 'd'] → DETACH (close CLI, keep daemon)
COMMAND → [recv '?'] → HELP → NORMAL
COMMAND → [recv other] → forward(0x02) + forward(byte) → NORMAL
```

### Injection format (hub pty input)
Text is written directly to the hub's pty master fd. Since the hub is running interactively, injected text appears as if the user typed it. Injections are terminated with `\n` to submit them as a user turn.

### Passthrough overlay
1. Daemon temporarily stops forwarding hub pty output to CLI connections
2. Daemon writes approval prompt directly to attached CLI ttys
3. Daemon reads single keypress from first attached CLI tty
4. Daemon writes response file
5. Daemon resumes normal relay

### Hook auto-approve for yolo (dynamic mode change)
`worker-pretool.sh` reads `thread.json` at invocation time:
```bash
MODE=$(jq -r .permissions_mode ~/.singleton/threads/$SINGLETON_THREAD_ID/thread.json)
if [ "$MODE" = "yolo" ]; then exit 0; fi
```

### Crash recovery
On daemon start:
```python
for task_dir in Path("~/.singleton/tasks").iterdir():
    task = read_task_json(task_dir)
    if task.status in ("running", "idle", "awaiting_approval"):
        if not pid_alive(task.pid):
            update_task_status(task.id, "done")
        else:
            # Mark as disconnected; user must cancel/restart manually
            update_task_status(task.id, "disconnected")
hub_session_id = read_hub_session_id()
spawn_hub(resume=hub_session_id)  # --resume flag
```

---

## Test Plan

| Layer | File | Tests |
|---|---|---|
| Unit | `test_store.py` | T-STORE-1 through T-STORE-13 |
| Unit/Integration | `test_worker.py` | T-WORKER-1 through T-WORKER-11 |
| Integration | `test_mcp.py` | T-MCP-1 through T-MCP-12 |
| Integration | `test_daemon.py` | T-DAEMON-1 through T-DAEMON-12 |
| Shell | `test_hooks.sh` or pytest subprocess | T-HOOKS-1 through T-HOOKS-7 |
| Manual | checklist below | T-CLI, T-FLOWS |

---

## Rollback / Mitigation

- All state is in `~/.singleton/`; deleting this dir returns to a clean state (workers will be orphaned but not harmful)
- No changes to user's existing Claude Code config during normal operation (hooks injected via `--settings`, not modifying project files)
- `setup.sh` writes `.claude/settings.json` in this repo only; does not touch global `~/.claude/settings.json`

---

## Completion Checklist

- [ ] `uv run pytest` passes all automated tests (T-STORE, T-WORKER, T-MCP, T-DAEMON, T-HOOKS)
- [ ] `./setup.sh` runs without errors on a clean environment
- [ ] T-FLOWS-1: `singleton` starts daemon and attaches to hub
- [ ] T-FLOWS-2: `/new-thread` creates supervised worker, worker appears in `/threads`
- [ ] T-FLOWS-3: Worker approval request injected into hub; hub approves autonomously
- [ ] T-FLOWS-4: Worker idle event injected as summary into hub
- [ ] T-FLOWS-7: `Ctrl+b d` detaches; `singleton attach` re-attaches; hub session intact
- [ ] T-FLOWS-8: Passthrough approval prompts user directly in terminal
- [ ] README.md documents setup and usage

---

## Proof Artifacts (to be filled in during implementation)

```
Command: uv run pytest
Result: [TBD]

Command: ./setup.sh
Result: [TBD]

Command: singleton → /new-thread → /threads
Output: [TBD]
```
