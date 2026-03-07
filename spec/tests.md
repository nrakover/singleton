# singleton — Test Inventory

Derived from spec.md and user_flows.md. Tests are organized by component. All automated tests live in `tests/`.

---

## T-STORE: State store (`test_store.py`)

| ID | Description | Type |
|---|---|---|
| T-STORE-1 | `create_thread` creates `thread.json` with correct fields (id, description, cwd, status=pending, permissions_mode, created_at) | unit |
| T-STORE-2 | `create_thread` with `cwd=None` sets cwd to `~/.singleton/workers/default/` | unit |
| T-STORE-3 | `list_threads` returns all threads sorted by created_at descending | unit |
| T-STORE-4 | `get_thread` returns full metadata including last_turn_summary | unit |
| T-STORE-5 | `update_thread_status` transitions status correctly and updates updated_at | unit |
| T-STORE-6 | Writing an event file creates `events/{event_id}.json` with correct schema | unit |
| T-STORE-7 | Writing a pending approval creates `pending/{req_id}.json` with correct schema | unit |
| T-STORE-8 | Writing a response creates `responses/{req_id}.json` with correct schema | unit |
| T-STORE-9 | `thread_output` pagination: page=0 returns last N lines; page=1 returns prior N lines; has_more is correct | unit |
| T-STORE-10 | `thread_output` with fewer lines than page_size returns all lines, has_more=False | unit |
| T-STORE-11 | `get_thread_events` pagination: page=0 returns latest N events; incrementing walks backwards | unit |
| T-STORE-12 | Concurrent writes to `output.txt` from multiple appends do not corrupt the file | unit |
| T-STORE-13 | `thread.json` is valid JSON after any store operation | unit |

---

## T-WORKER: Worker process management (`test_worker.py`)

| ID | Description | Type |
|---|---|---|
| T-WORKER-1 | `spawn_worker` launches subprocess with correct CLI flags for `yolo` mode (`--dangerously-skip-permissions`) | unit |
| T-WORKER-2 | `spawn_worker` launches subprocess without `--dangerously-skip-permissions` for `supervised` and `passthrough` modes | unit |
| T-WORKER-3 | `spawn_worker` injects per-thread hooks via `--settings` flag (not modifying project settings) | unit |
| T-WORKER-4 | `spawn_worker` sets CWD to specified directory | unit |
| T-WORKER-5 | `send_turn` writes correct stream-json user-turn format to worker stdin | unit |
| T-WORKER-6 | `send_turn` reads and parses `result` event from worker stdout | unit |
| T-WORKER-7 | `send_turn` extracts assistant text content (ignores tool_use blocks) for summary | unit |
| T-WORKER-8 | `send_turn` returns result_text truncated to ≤500 chars when longer | unit |
| T-WORKER-9 | `cancel_worker` sends SIGTERM to worker process | unit |
| T-WORKER-10 | Worker output is appended to `output.txt` after each turn | unit |
| T-WORKER-11 | Mock stream-json worker receives multiple sequential turns correctly | integration |

---

## T-DAEMON: Daemon event loop and injection (`test_daemon.py`)

| ID | Description | Type |
|---|---|---|
| T-DAEMON-1 | File watcher detects new event file in `threads/{id}/events/` within 500ms | unit |
| T-DAEMON-2 | Injection is queued when `hub_busy=True` | unit |
| T-DAEMON-3 | Queued injection fires when hub output goes quiet for >200ms | unit |
| T-DAEMON-4 | Up to 10 injections can be queued; 11th is logged and dropped | unit |
| T-DAEMON-5 | Worker `Stop` event triggers summary injection into hub pty | integration |
| T-DAEMON-6 | Worker `supervised` approval event triggers approval injection into hub pty | integration |
| T-DAEMON-7 | Passthrough approval suspends pty relay, prompts user tty, resumes relay after response | integration |
| T-DAEMON-8 | Daemon writes `daemon.pid` on start; removes it on clean stop | unit |
| T-DAEMON-9 | Daemon binds Unix socket at `~/.singleton/daemon.sock`; removes it on clean stop | unit |
| T-DAEMON-10 | MCP HTTP server starts on configured port; responds to `GET /health` | unit |
| T-DAEMON-11 | Multiple CLI connections receive same hub pty output (fan-out) | integration |
| T-DAEMON-12 | Input from any attached CLI connection is forwarded to hub pty | integration |

---

## T-MCP: MCP tool behavior (`test_mcp.py`)

| ID | Description | Type |
|---|---|---|
| T-MCP-1 | `create_thread` returns `{thread_id}` and thread appears in `list_threads` | integration |
| T-MCP-2 | `list_threads` returns empty list when no threads exist | unit |
| T-MCP-3 | `get_thread` returns 404-equivalent error for unknown thread_id | unit |
| T-MCP-4 | `thread_output` returns correct paginated lines with proper `has_more` flag | integration |
| T-MCP-5 | `get_thread_events` returns paginated events newest-first | integration |
| T-MCP-6 | `send_to_thread` returns `result_text` from mock worker | integration |
| T-MCP-7 | `cancel_thread` updates thread status to `cancelled` | integration |
| T-MCP-8 | `set_thread_permissions` updates `thread.json` permissions_mode | unit |
| T-MCP-9 | `list_pending_approvals` returns only unresolved requests | unit |
| T-MCP-10 | `approve_tool_call` writes approve response file; hook unblocks | integration |
| T-MCP-11 | `deny_tool_call` writes deny response file; hook exits 2 | integration |
| T-MCP-12 | `set_thread_permissions` to `yolo` causes subsequent `PreToolUse` hook to auto-approve | integration |

---

## T-HOOKS: Hook scripts (`test_hooks.sh` / shell-based)

| ID | Description | Type |
|---|---|---|
| T-HOOKS-1 | `worker-stop.sh` writes stop event file with correct schema | unit |
| T-HOOKS-2 | `worker-pretool.sh` writes pending request file when mode is `supervised` | unit |
| T-HOOKS-3 | `worker-pretool.sh` polls response file and exits 0 when approved | unit |
| T-HOOKS-4 | `worker-pretool.sh` polls response file and exits 2 when denied | unit |
| T-HOOKS-5 | `worker-pretool.sh` exits 2 after timeout (300 iterations) | unit |
| T-HOOKS-6 | `worker-pretool.sh` in `yolo` mode exits 0 immediately without writing files | unit |
| T-HOOKS-7 | `worker-notify.sh` writes notification event file with correct schema | unit |

---

## T-CLI: CLI behavior (manual / integration)

| ID | Description | Type |
|---|---|---|
| T-CLI-1 | `singleton` with no daemon running starts daemon and attaches | manual |
| T-CLI-2 | `singleton` with daemon running attaches without starting a new daemon | manual |
| T-CLI-3 | `singleton attach` is equivalent to `singleton` when daemon is running | manual |
| T-CLI-4 | `singleton status` prints thread status board and exits without attaching | manual |
| T-CLI-5 | `singleton stop` stops daemon, hub, and all workers gracefully | manual |
| T-CLI-6 | `Ctrl+b d` detaches CLI without disrupting hub or workers | manual |
| T-CLI-7 | `Ctrl+b ?` prints prefix command help in relay | manual |
| T-CLI-8 | Non-prefix control sequences (e.g. `Ctrl+c`, `Ctrl+l`) are forwarded to hub | manual |
| T-CLI-9 | Second terminal running `singleton attach` receives same hub output | manual |
| T-CLI-10 | Input from second attached terminal reaches hub | manual |

---

## T-FLOWS: End-to-end user flows (manual)

| ID | Flow | Covers |
|---|---|---|
| T-FLOWS-1 | UF-1: First-time setup and launch | setup.sh, daemon start, hub attach |
| T-FLOWS-2 | UF-2: Create supervised thread via `/new-thread` | skill, create_thread, worker spawn |
| T-FLOWS-3 | UF-3: Worker approval request handled by hub autonomously | PreToolUse hook, injection, approve_tool_call |
| T-FLOWS-4 | UF-4: Worker completes turn; hub receives summary injection | Stop hook, result parsing, injection |
| T-FLOWS-5 | UF-5: Hub inspects thread output across multiple pages | thread_output pagination |
| T-FLOWS-6 | UF-6: Hub sends follow-up to idle worker | send_to_thread |
| T-FLOWS-7 | UF-7: Detach with Ctrl+b d; re-attach with singleton | pty persistence, CLI relay |
| T-FLOWS-8 | UF-9: Passthrough thread with direct user approval | passthrough mode, pty relay suspend |
| T-FLOWS-9 | UF-10: `/threads` shows status board with pending approvals | list_threads, list_pending_approvals, skill |
| T-FLOWS-10 | UF-12: Permission mode changed mid-thread from supervised to yolo | set_thread_permissions, hook mode check |
| T-FLOWS-11 | UF-13: Daemon crash recovery; threads intact after restart | crash recovery, hub resume |
