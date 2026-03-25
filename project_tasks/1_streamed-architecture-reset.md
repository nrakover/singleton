# Task 1: Streamed Architecture Reset

## Objective

Replace the bootstrap architecture with a cleaner split between a daemon-owned streamed hub session and durable, one-run-per-process worker sessions. Preserve the product behavior where appropriate, but rewrite the implementation around direct Python hooks, SQLite-backed worker coordination, JSONL session logs, and a singleton-owned multi-attach TUI.

---

## Requirements

### Functional

- Hub runs as a daemon-owned long-lived `claude -p` session with streaming input/output
- Workers run as one-request-per-process `claude -p` sessions and resume with `session_id`
- Worker lifecycle facts are emitted through hooks into SQLite, not derived from file drops
- Permissions use `PermissionRequest` hooks plus Claude-native permission config to differentiate `yolo`, `supervised`, and `passthrough`
- Hook commands invoke Python entrypoints directly, without bash wrapper scripts
- Full-fidelity worker stream output is written to JSONL log files by the session manager, not by hooks
- `singleton` owns a minimal TUI that can render hub output, thread/run lifecycle, permission requests, and generic fallback events
- Multi-attach remains supported with mirrored views and a single input owner for hub typing

### Non-functional

- SQLite writes should be sparse and fast; prefer append-only durable records where possible
- Runtime abstractions should be testable through protocols and simple fake implementations, not magic mocks
- Spec, interfaces, user flows, tests, task plan, and project memory must remain synchronized
- Follow TDD for execution work after the spec rewrite lands

---

## Agreed Design Decisions

- The daemon is still a singleton. We do not introduce a durable command queue for daemon-originated worker requests.
- The hub and daemon communicate in memory only; if the daemon dies, the hub dies too.
- Worker sessions may outlive the daemon. Hooks must keep writing durable lifecycle messages into SQLite even while the daemon is down.
- Worker sessions are one subprocess run per request. Follow-up requests spawn a new process with `--resume <session_id>`.
- Hooks inject `SINGLETON_THREAD_ID` and `SINGLETON_RUN_ID` directly into the worker environment.
- `SessionStart`, `PermissionRequest`, `Stop`, and `StopFailure` are the authoritative worker lifecycle hooks.
- Worker stdout/stderr streams are still captured to JSONL logs, but parsing is not the primary source of lifecycle truth.
- Per-client attach state is ephemeral and not persisted. The daemon rebuilds canonical runtime state from SQLite + logs on restart.

---

## Proposed Durable Model

### SQLite tables

#### `threads`

- `thread_id`
- `description`
- `context`
- `cwd`
- `permissions_mode`
- `session_id` nullable
- `created_at`
- `updated_at`

#### `runs`

- `run_id`
- `thread_id`
- `created_at`
- `pid` nullable
- `finished_at` nullable
- `exit_code` nullable

#### `messages`

- `message_id`
- `direction` (`to_worker` | `from_worker`)
- `message_type`
- `thread_id`
- `run_id`
- `payload_json`
- `created_at`

### Message types

- `run_started`
- `permission_request`
- `permission_resolution`
- `run_finished`
- `notification`

`permission_resolution` payload includes optional `reason` so deny feedback can be passed back to Claude.

---

## Runtime Components

### Hub controller

- Owns the long-lived hub subprocess
- Streams input/output in memory only
- Feeds semantic UI events to attached TUI clients
- Dies with the daemon

### Worker session manager

- Creates a `runs` row before spawning any worker subprocess
- Spawns one `claude -p` worker per request, resuming with `thread.session_id` when present
- Wires direct Python hooks and injects `SINGLETON_THREAD_ID` / `SINGLETON_RUN_ID`
- Tees stdout/stderr to JSONL log files
- Updates sparse durable fields such as `threads.session_id`, `runs.pid`, `runs.finished_at`, and `runs.exit_code`
- Uses subprocess exit observation only as fallback reconciliation for abnormal termination cases not covered by hooks

### SQLite message repository

- Appends worker-originated lifecycle facts from hooks
- Appends daemon-originated `permission_resolution` facts
- Supports efficient queries for unresolved permission requests, recent run lifecycle events, and daemon restart reconciliation

### TUI runtime

- Maintains canonical app state in daemon memory
- Supports multiple attached clients
- Mirrors output to all clients
- Enforces a single input owner for hub typing, with explicit handoff

---

## File-Level Plan

Likely targets during execution:

- `src/singleton/store.py` - SQLite-backed repositories and durable metadata
- `src/singleton/hooks.py` - direct Python hook command generation
- `src/singleton/hub.py` - long-lived streamed hub process controller
- `src/singleton/worker.py` - one-run worker session manager + log teeing
- `src/singleton/daemon.py` - in-memory app state, attach management, recovery, hub ownership
- `src/singleton/cli.py` - client attach flow and local terminal integration
- `src/singleton/tui.py` (new) - minimal renderer/controller abstractions
- `hooks/` scripts removed or replaced by Python module entrypoints
- `tests/` reorganized around repositories, hooks, session manager, hub controller, TUI, and recovery

---

## Test Plan

### Phase 1: spec + repository

- SQLite schema creation and migration-free bootstrap
- Sparse metadata updates for threads/runs
- Append-only message insertion and querying

### Phase 2: hook entrypoints

- `SessionStart` emits `run_started`
- `PermissionRequest` emits `permission_request` and waits for matching resolution
- `Stop` emits successful `run_finished`
- `StopFailure` emits failed `run_finished`

### Phase 3: worker session manager

- Creates run row before spawn
- Injects run/thread env vars into hooks
- Uses `--resume <session_id>` for follow-up runs
- Tees stdout/stderr to JSONL logs
- Reconciles abnormal exits cleanly

### Phase 4: hub + TUI

- Long-lived hub process with streaming input/output
- Multi-attach mirrored rendering with single input owner
- Pending permission requests rendered from SQLite-backed daemon state

### Phase 5: integration and recovery

- Daemon restart while worker continues running
- Hooks continue writing durable events while daemon is down
- Daemon rebuilds state from SQLite and logs on restart

### Manual verification gate

- Real terminal attach/detach behavior must be manually re-verified before closing any major implementation phase
- Real multi-attach ownership handoff must be manually re-verified when daemon/TUI input routing changes
- Real passthrough permission behavior must be manually re-verified when permission handling changes
- Real daemon-crash / worker-survival behavior must be manually re-verified when recovery logic changes

---

## Rollback / Mitigation

- Keep the architectural reset isolated to new abstractions first, then retire obsolete code paths once replacement tests pass
- Preserve durable user-visible semantics in the spec before deleting old implementations
- Prefer replacing file-drop IPC with SQLite in a single vertical slice so partial migrations do not leave mixed coordination paths in production

---

## Completion Checklist

- Spec artifacts rewritten and internally consistent
- Project memory updated with the new architecture
- New repository, hook, session-manager, and TUI tests written before or alongside implementation
- Manual verification gate from `spec/tests.md` re-run for the changed scope
- Full validation passes:
  - `uv run pytest`
  - `uv run ruff format .`
  - `uv run ruff check src/ tests/`
  - `uv run ty check`
