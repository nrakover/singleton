# singleton - Test Inventory

Derived from the rewritten spec. Tests should favor small protocols and fake implementations over magic mocks.

---

## T-REPO: SQLite repositories (`test_store.py`)

| ID | Description | Type |
|---|---|---|
| T-REPO-1 | Schema bootstrap creates `threads`, `runs`, and `messages` tables | unit |
| T-REPO-2 | `create_thread` persists default worker cwd when `cwd=None` | unit |
| T-REPO-3 | `create_run` persists a run row before process launch | unit |
| T-REPO-4 | `append_message` persists `direction`, `message_type`, `thread_id`, `run_id`, and payload JSON | unit |
| T-REPO-5 | Updating `threads.session_id` touches only sparse metadata fields | unit |
| T-REPO-6 | Updating `runs.pid`, `runs.finished_at`, and `runs.exit_code` leaves other durable data untouched | unit |
| T-REPO-7 | `list_pending_approvals` derives unresolved requests from `permission_request` minus matching `permission_resolution` | unit |
| T-REPO-8 | `get_thread_events` paginates newest-first durable messages for one thread | unit |
| T-REPO-9 | Concurrent append-heavy message writes do not corrupt SQLite state | unit |

---

## T-HOOKS: Direct Python hooks (`test_hooks.py`)

| ID | Description | Type |
|---|---|---|
| T-HOOKS-1 | `SessionStart` hook writes `run_started` with `session_id` and `run_id` | unit |
| T-HOOKS-2 | `PermissionRequest` hook writes `permission_request` with tool input and suggestions | unit |
| T-HOOKS-3 | `PermissionRequest` hook returns allow when matching `permission_resolution` is approved | unit |
| T-HOOKS-4 | `PermissionRequest` hook returns deny with freeform reason when matching resolution is denied | unit |
| T-HOOKS-5 | `PermissionRequest` hook times out safely when no resolution arrives | unit |
| T-HOOKS-6 | `Stop` hook writes `run_finished` with `outcome=completed` and `last_assistant_message` | unit |
| T-HOOKS-7 | `StopFailure` hook writes `run_finished` with `outcome=api_error`, `error`, and `error_details` | unit |
| T-HOOKS-8 | `Notification` hook writes `notification` message | unit |

---

## T-WORKER: Worker session manager (`test_worker.py`)

| ID | Description | Type |
|---|---|---|
| T-WORKER-1 | New run row is created before worker spawn | unit |
| T-WORKER-2 | Spawn command injects direct Python hooks instead of bash wrapper scripts | unit |
| T-WORKER-3 | Spawn command includes `SINGLETON_THREAD_ID` and `SINGLETON_RUN_ID` env vars | unit |
| T-WORKER-4 | Follow-up run uses `--resume <session_id>` when thread metadata contains one | unit |
| T-WORKER-5 | `yolo` mode uses Claude-native bypass permissions while other modes do not | unit |
| T-WORKER-6 | Stdout stream is teed to `{run_id}.stdout.jsonl` | unit |
| T-WORKER-7 | Stderr stream is teed to `{run_id}.stderr.jsonl` | unit |
| T-WORKER-8 | Abnormal subprocess exit without `run_finished` is reconciled as fallback failure state | integration |
| T-WORKER-9 | Worker run updates `threads.session_id` from hook-authored lifecycle data | integration |

---

## T-HUB: Hub controller (`test_hub.py`)

| ID | Description | Type |
|---|---|---|
| T-HUB-1 | Hub process launches as daemon-owned long-lived streamed session | unit |
| T-HUB-2 | Hub controller writes prompts to hub stdin and receives streamed output in memory | unit |
| T-HUB-3 | Hub MCP config is written to `.mcp.json`, not `settings.json` | unit |
| T-HUB-4 | Singleton MCP tools are pre-allowed in hub settings | unit |

---

## T-DAEMON: Daemon state and recovery (`test_daemon.py`)

| ID | Description | Type |
|---|---|---|
| T-DAEMON-1 | Daemon writes `daemon.pid` and binds `daemon.sock` on start | unit |
| T-DAEMON-2 | Daemon serves MCP on configured port | unit |
| T-DAEMON-3 | Daemon rebuilds unresolved permission requests from SQLite on restart | integration |
| T-DAEMON-4 | Daemon restart does not require worker subprocesses to stop first | integration |
| T-DAEMON-5 | Worker hooks continue appending durable messages while daemon is down | integration |
| T-DAEMON-6 | Daemon surfaces `run_finished` from hook-authored messages rather than stdout parsing | integration |
| T-DAEMON-7 | `approve_tool_call` appends `permission_resolution` and unblocks the hook | integration |
| T-DAEMON-8 | `deny_tool_call` appends `permission_resolution` with reason and blocks the hook | integration |
| T-DAEMON-9 | Daemon rebuilds canonical runtime state from durable worker-plane data for fresh attaches | integration |

---

## T-TUI: Multi-attach rendering (`test_tui.py`)

| ID | Description | Type |
|---|---|---|
| T-TUI-1 | Multiple attached clients receive mirrored rendered state | integration |
| T-TUI-2 | Exactly one client owns freeform hub input at a time | integration |
| T-TUI-3 | Non-owning clients can observe but not type into the hub | integration |
| T-TUI-4 | Control handoff updates ownership cleanly | integration |
| T-TUI-5 | Passthrough approval is routed to the active input owner | integration |
| T-TUI-6 | Detach removes only that client, not daemon or hub state | integration |

---

## T-MCP: MCP behavior (`test_mcp.py`)

| ID | Description | Type |
|---|---|---|
| T-MCP-1 | `create_thread` returns `{thread_id}` and persists durable thread metadata | integration |
| T-MCP-2 | `send_to_thread` creates a run and returns terminal summary from `run_finished` | integration |
| T-MCP-3 | `thread_output` paginates JSONL-backed run output correctly | integration |
| T-MCP-4 | `get_thread_events` returns durable lifecycle messages newest-first | integration |
| T-MCP-5 | `set_thread_permissions` updates future-run permission mode | unit |
| T-MCP-6 | `list_pending_approvals` returns unresolved permission requests only | integration |
| T-MCP-7 | `deny_tool_call(request_id, reason)` persists deny reason for hook consumption | integration |
| T-MCP-8 | `cancel_thread` cancels the active run for a thread when one exists | integration |

---

## T-MANUAL: Manual verification gate

These checks are intentionally manual because they rely on real terminal behavior, real Claude Code process interaction, or multi-client UX details that are difficult to verify programmatically with acceptable confidence.

These manual checks must be re-run by the coding agent before marking any major implementation task complete, in addition to the required automated validation commands.

| ID | Description | Type |
|---|---|---|
| T-MANUAL-1 | Real `singleton` attach launches the daemon, hub, and TUI cleanly in a real terminal | manual |
| T-MANUAL-2 | Real `Ctrl+b d` detaches only the current client and leaves daemon state intact | manual |
| T-MANUAL-3 | Real multi-attach mirrors state across two terminals and enforces single input ownership | manual |
| T-MANUAL-4 | Real passthrough permission prompt is routed to the active input owner and returns the decision to Claude | manual |
| T-MANUAL-5 | Real daemon crash/restart preserves in-flight worker durability through SQLite and logs | manual |

---

## T-CLI: Manual behavior

| ID | Description | Type |
|---|---|---|
| T-CLI-1 | `singleton` starts daemon and attaches when not running | manual |
| T-CLI-2 | `singleton attach` adds a second mirrored client | manual |
| T-CLI-3 | `singleton status` prints thread and approval summary without attaching | manual |
| T-CLI-4 | `Ctrl+b d` detaches only the current client | manual |
| T-CLI-5 | Input ownership can be handed from one terminal to another | manual |

---

## T-FLOWS: End-to-end flows

| ID | Flow | Covers |
|---|---|---|
| T-FLOWS-1 | UF-1 setup and launch | setup, daemon, hub, TUI |
| T-FLOWS-2 | UF-3 start work on a thread | run creation, worker spawn, completion |
| T-FLOWS-3 | UF-4 supervised permission request | `PermissionRequest`, MCP approval |
| T-FLOWS-4 | UF-5 passthrough permission request | active-owner routing |
| T-FLOWS-5 | UF-9 follow-up run resumes prior session | `session_id` continuity |
| T-FLOWS-6 | UF-11 multi-attach ownership handoff | TUI ownership rules |
| T-FLOWS-7 | UF-12 daemon crash while worker continues | recovery from SQLite + logs |

---

## Coverage Notes

- `spec/spec.md` sections 2-8 are covered by T-REPO, T-HOOKS, T-WORKER, T-HUB, T-DAEMON, T-TUI, and T-MCP.
- `spec/spec.md` sections 3 and the real-terminal aspects of sections 6-8 also require the T-MANUAL gate.
- Every major task completion requires:
  - all automated validation commands to pass
  - all relevant automated tests for the changed scope to pass
  - the T-MANUAL checks that exercise changed real-terminal or real-Claude behavior to be re-verified
