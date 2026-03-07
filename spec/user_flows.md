# singleton — User Flows

## UF-1: First-time setup

**Actor**: User who has just cloned the repo.

1. User runs `./setup.sh` in the repo root
2. Setup installs Python dependencies via `uv sync`
3. Setup creates `~/.singleton/workers/default/` directory
4. Setup writes `.claude/settings.json` with MCP server configuration pointing to the daemon's default HTTP port
5. Setup makes hook scripts executable (`chmod +x hooks/*.sh`)
6. User runs `singleton` — daemon starts, hub launches, user is attached to hub pty

**Outcome**: User is in the hub REPL with the singleton MCP server available.

---

## UF-2: Create a new background thread (supervised, default)

**Actor**: User in hub session.

1. User types `/new-thread`
2. Hub skill prompts: "Describe the thread:"
3. User provides description (e.g., "Refactor the auth module in ~/repos/myapp")
4. Hub skill prompts: "Working directory? (default: ~/.singleton/workers/default/)" — user provides `/Users/me/repos/myapp`
5. Hub skill prompts: "Permissions mode? [supervised/yolo/passthrough] (default: supervised)" — user accepts default
6. Hub calls `create_thread("Refactor auth module", cwd="/Users/me/repos/myapp", permissions_mode="supervised")`
7. Hub confirms: "Thread abc123 created. Worker is running in supervised mode in /Users/me/repos/myapp."
8. Worker spawns in background; begins working autonomously

**Outcome**: Thread is running in the background. Hub is ready for other interactions.

---

## UF-3: Worker requests tool approval (supervised mode)

**Actor**: Background worker needing tool approval; hub session is idle.

1. Worker's `PreToolUse` hook fires when worker attempts to run `Bash('git commit -am "refactor"')`
2. Hook writes approval request; daemon detects event via file watcher
3. Daemon injects into hub pty (hub is idle, so injection fires immediately):
   ```
   [TASK abc123 — awaiting approval] Bash('git commit -am "refactor"')
   Call approve_tool_call("req_7f3a") or deny_tool_call("req_7f3a")
   ```
4. Hub agent reads context, decides this is safe, calls `approve_tool_call("req_7f3a")`
5. Worker's hook receives approval, exits `0`, git commit proceeds
6. Worker continues execution

**Outcome**: Worker's tool call is approved without any user interaction. Hub handled it autonomously.

---

## UF-4: Worker completes a turn

**Actor**: Background worker that has finished a unit of work.

1. Worker completes its current turn; emits `result` event on stdout
2. Daemon reads the `result` event, extracts assistant text
3. Daemon injects summary into hub pty (queued if hub is busy, fires when idle):
   ```
   [TASK abc123 — idle] "Refactor auth module"
   Result: Extracted auth logic into AuthService class, updated 4 files, added unit tests. All tests pass.
   Use thread_output("abc123") or send_to_thread("abc123", ...) for details.
   ```
4. Hub processes the injection, optionally acts (e.g., sends follow-up or just notes completion)

**Outcome**: Hub is informed of completion with minimal context. User sees result in hub conversation.

---

## UF-5: Hub agent inspects thread output

**Actor**: Hub agent that wants to review a worker's work in detail.

1. Hub calls `thread_output("abc123", page=0, page_size=50)`
2. Receives last 50 lines of output
3. If more context needed, hub calls `thread_output("abc123", page=1, page_size=50)` for the next 50 lines back in history
4. Hub can also call `get_thread_events("abc123", page=0, page_size=10)` for structured event log (tool calls, errors)

**Outcome**: Hub can progressively inspect worker output without dumping everything into context at once.

---

## UF-6: Hub sends a follow-up message to a worker

**Actor**: Hub agent following up on a completed thread.

1. Worker is in `idle` state
2. Hub calls `send_to_thread("abc123", "Also update the README with API documentation")`
3. Daemon writes stream-json user turn to worker's stdin; worker begins processing
4. Hub blocks until worker returns result
5. Daemon injects result summary into hub pty
6. Hub receives the summary as next turn context

**Outcome**: Hub can extend a thread's work without creating a new thread, using conversational continuity.

---

## UF-7: User detaches and re-attaches

**Actor**: User who wants to step away while threads run.

1. User presses `Ctrl+b d` in the singleton terminal
2. CLI intercepts the sequence (does not forward to hub), detaches gracefully
3. Hub session and daemon continue running; workers continue in background
4. Later, user runs `singleton` (or `singleton attach`)
5. CLI reconnects to daemon via Unix socket, re-attaches to hub pty
6. User sees any output that was generated while away (hub conversation has continued)

**Outcome**: User can safely detach without disrupting any ongoing work.

---

## UF-8: Multi-terminal attach

**Actor**: User with two terminal windows.

1. Terminal A: user is attached via `singleton`
2. Terminal B: user runs `singleton attach`
3. Both terminals show identical hub output (daemon fans pty output to both)
4. Input from either terminal is forwarded to the hub

**Outcome**: Hub can be observed/interacted with from multiple terminals simultaneously.

---

## UF-9: Thread with passthrough permissions

**Actor**: User creating a thread that will involve sensitive operations.

1. User creates a thread with `permissions_mode="passthrough"`
2. Worker begins; at some point attempts a sensitive operation (e.g., `Bash('curl ... | bash')`)
3. Hook writes approval request; daemon detects event
4. Daemon suspends pty relay; writes directly to user's terminal:
   ```
   [TASK abc123] Bash('curl https://example.com/install.sh | bash')
   Approve? [a/d]:
   ```
5. User types `a` or `d`
6. Daemon writes response, resumes pty relay
7. Worker's hook unblocks accordingly

**Outcome**: User has direct control over sensitive tool calls, bypassing hub judgment.

---

## UF-10: View thread status board

**Actor**: User wanting an overview of all threads.

1. User types `/threads` in hub
2. Hub calls `list_threads()` and `list_pending_approvals()`
3. Hub renders status board, e.g.:
   ```
   Threads (4):
     abc123  [idle]              Refactor auth module       /repos/myapp
     def456  [running]           Write API docs             /repos/myapp
     ghi789  [awaiting_approval] Deploy staging             /repos/infra    ← NEEDS ATTENTION
     jkl012  [done]              Fix login bug              /repos/myapp

   Pending approvals:
     req_5f2a  ghi789  Bash('kubectl apply -f staging.yaml')
   ```
4. Hub may autonomously approve `req_5f2a` if it judges it safe, or ask user

**Outcome**: User has a clear picture of what's running and what needs attention.

---

## UF-11: Cancel a running thread

**Actor**: Hub agent or user who wants to stop a thread.

1. Hub calls `cancel_thread("abc123")`
2. Daemon sends SIGTERM to worker process
3. Worker status updated to `cancelled` in `thread.json`
4. Hub confirms cancellation

**Outcome**: Worker is stopped cleanly.

---

## UF-12: Change thread permissions mid-flight

**Actor**: Hub agent that decides a thread should be more or less supervised.

1. Thread `abc123` is running in `supervised` mode
2. Hub determines the thread is running cleanly and escalates to `yolo`:
   `set_thread_permissions("abc123", "yolo")`
3. Daemon updates `thread.json`; next worker turn spawns without `--dangerously-skip-permissions`... wait: mode change takes effect on next hook invocation, which reads mode from `thread.json`

**Note**: `yolo` mode uses `--dangerously-skip-permissions` flag at spawn time. Dynamically switching from `supervised` to `yolo` does not restart the process — it instead causes the `PreToolUse` hook to auto-approve all subsequent requests (hook reads mode from `thread.json` and returns early with exit 0). Switching to `yolo` from `passthrough`/`supervised` means the hook will auto-approve.

**Outcome**: Permission mode changes without restarting the worker.

---

## UF-13: Daemon crash recovery

**Actor**: System where daemon has crashed or been restarted.

1. User runs `singleton`
2. Daemon starts fresh; reads `~/.singleton/threads/*/thread.json`
3. For each thread with a PID, daemon checks if PID is alive
4. Alive workers: daemon attempts to re-attach pipes (if possible); otherwise marks `disconnected`
5. Dead workers: daemon updates status to `done` or `cancelled` as appropriate
6. Daemon reads `hub_session_id`, starts hub with `--resume {id}` to restore conversation
7. User attaches and sees previous hub conversation continued

**Outcome**: Minimal disruption from daemon restart; thread state is preserved.

---

## UF-14: Default worker configuration

**Actor**: User who wants all workers to default to a specific model or tools.

1. User creates `~/.singleton/workers/default/.claude/settings.json`:
   ```json
   {
     "model": "claude-opus-4-6",
     "allowedTools": ["Bash", "Read", "Edit", "Write", "Glob", "Grep"]
   }
   ```
2. All subsequent `create_thread` calls without an explicit `cwd` use these settings
3. Workers run with the specified model and tool restrictions by default

**Outcome**: User can centrally configure default worker behavior without per-thread configuration.
