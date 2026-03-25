# singleton - User Flows

## UF-1: First-time setup

**Actor**: User who has just cloned the repo.

1. User runs `./setup.sh`
2. Setup installs dependencies and creates `~/.singleton/workers/default/`
3. Setup ensures singleton runtime directories exist
4. User runs `singleton`
5. Daemon starts, hub launches, and the singleton TUI attaches

**Outcome**: User is attached to the daemon-owned hub through the singleton TUI.

---

## UF-2: Create a new supervised thread

**Actor**: User in the hub session.

1. User asks the hub to create a worker thread
2. Hub calls `create_thread(...)`
3. Daemon persists thread metadata
4. Hub confirms thread creation

**Outcome**: Durable thread metadata exists, but no idle worker process is kept alive.

---

## UF-3: Start work on a thread

**Actor**: Hub agent delegating work.

1. Hub calls `send_to_thread("abc123", "Refactor the auth module")`
2. Daemon creates a `run` record first
3. Daemon spawns a fresh worker subprocess, resuming with the thread's `session_id` if present
4. `SessionStart` hook emits `run_started`
5. Worker processes the request and eventually emits `run_finished` via `Stop` or `StopFailure`
6. `send_to_thread` returns the terminal summary

**Outcome**: One run is completed for the thread, and the thread remains resumable for future work.

---

## UF-4: Worker requests permission in supervised mode

**Actor**: Worker run and hub.

1. Worker hits a Claude permission boundary
2. `PermissionRequest` hook writes a durable `permission_request` message into SQLite
3. Daemon reflects the pending approval into hub/TUI state
4. Hub agent decides to approve or deny via MCP
5. Daemon writes `permission_resolution`
6. Hook observes the resolution and returns the corresponding decision to Claude

**Outcome**: The worker is supervised by the hub without requiring file-drop IPC.

---

## UF-5: Worker requests permission in passthrough mode

**Actor**: Worker run and active attached user.

1. Worker hits a Claude permission boundary
2. `PermissionRequest` hook writes a durable `permission_request`
3. Daemon routes the request to the active input owner in the TUI
4. User approves or denies, optionally with a reason
5. Daemon writes `permission_resolution`
6. Hook returns that decision to Claude

**Outcome**: Sensitive operations are decided directly by the user.

---

## UF-6: Worker completes successfully

**Actor**: Worker run that has finished.

1. Worker completes the request
2. `Stop` hook emits `run_finished` with `outcome="completed"`, `session_id`, and `last_assistant_message`
3. Daemon updates thread/run metadata
4. Hub and TUI show the summary

**Outcome**: Completion is durable even if the daemon restarts shortly afterward.

---

## UF-7: Worker ends with API failure

**Actor**: Worker run affected by an API-level error.

1. Claude ends with an API error
2. `StopFailure` hook emits `run_finished` with `outcome="api_error"`
3. Daemon surfaces the failure to hub/TUI state
4. Hub may retry or ask the user what to do next

**Outcome**: API failures are first-class terminal run events.

---

## UF-8: Hub inspects worker artifacts

**Actor**: Hub agent reviewing worker output.

1. Hub calls `thread_output("abc123", page=0, page_size=50)`
2. Receives recent JSONL-derived output lines
3. If needed, hub calls `get_thread_events("abc123")` for durable lifecycle events

**Outcome**: Hub can inspect detail without flooding its context by default.

---

## UF-9: Follow up on a prior thread

**Actor**: Hub agent continuing work.

1. Thread `abc123` already has a `session_id`
2. Hub calls `send_to_thread("abc123", "Also update the README")`
3. Daemon creates a new run and spawns `claude -p --resume <session_id>`
4. Worker completes and updates the thread's latest `session_id` if needed

**Outcome**: Thread continuity is preserved without long-lived idle workers.

---

## UF-10: User detaches and re-attaches

**Actor**: User stepping away temporarily.

1. User presses `Ctrl+b d`
2. The current client detaches
3. Daemon and hub continue running
4. Later the user runs `singleton attach`
5. The daemon sends the current view model to the new client

**Outcome**: Clients are ephemeral; daemon state persists.

---

## UF-11: Multi-attach with ownership handoff

**Actor**: User with two terminals.

1. Terminal A is attached and owns hub input
2. Terminal B runs `singleton attach`
3. Both terminals render the same app state
4. Terminal B attempts to type into the hub and is blocked until it takes control
5. Terminal B issues a control handoff command
6. Daemon grants ownership to Terminal B

**Outcome**: Multi-attach is supported without ambiguous concurrent hub input.

---

## UF-12: Daemon crashes while a worker run continues

**Actor**: System under failure.

1. A worker run is active
2. Daemon crashes
3. Hub is lost, but the worker subprocess continues
4. Worker hooks continue writing lifecycle and permission messages into SQLite
5. User restarts `singleton`
6. Daemon rebuilds state from SQLite and resumes managing unresolved worker activity

**Outcome**: Real work survives daemon failure.

---

## UF-13: Default worker configuration

**Actor**: User who wants shared worker defaults.

1. User creates `~/.singleton/workers/default/.claude/settings.json`
2. Threads created without an explicit `cwd` use that directory
3. Worker runs inherit those Claude Code defaults plus singleton-injected hooks

**Outcome**: Worker defaults remain user-configurable without mutating per-project settings.
