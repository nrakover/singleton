# singleton - User Flows

## 0. Install and Configure Copilot CLI

### Goal

Install singleton for Copilot CLI without requiring the user to know where
Copilot stores MCP configuration.

### Flow

1. User runs `copilot plugin marketplace add nrakover/singleton`.
2. User runs `copilot plugin install singleton@singleton`.
3. Copilot installs the plugin and loads its `singleton` MCP server definition.
4. On first MCP server start, the plugin launcher resolves the platform.
5. The launcher downloads the latest release archive and checksum into a
   temporary directory, verifies the checksum, and installs the binary into
   `${COPILOT_PLUGIN_DATA}/bin`.
6. The launcher execs `singleton serve --stdio --backend copilot`.
7. `singleton serve --stdio` starts or reuses the local daemon and proxies MCP
   stdio to the daemon socket.

### Expected behavior

- Plugin bootstrap diagnostics go to stderr, never MCP stdout.
- Repeated starts reuse the installed binary unless `SINGLETON_FORCE_INSTALL=1`
  or `SINGLETON_BINARY` overrides the path.
- Users can pin a release with `SINGLETON_VERSION`.
- Users can bypass the plugin with `singleton install-mcp --client copilot` or
  manual `singleton mcp-config` output.
- Repeated foreground starts reuse the same daemon. The daemon is auto-started
  under a per-database startup lock and in a separate Unix process group so
  ordinary foreground-client shutdown does not terminate background turns.
- If lifecycle files are stale, `singleton status` identifies the stale pid or
  socket and prints the `singleton stop --database ...` cleanup command.
- Direct repository plugin installs are deprecated by Copilot CLI, so the
  marketplace flow is the canonical install path.

---

## 0.1 Direct MCP Client Registration

### Goal

Configure a foreground agent client after `singleton` is already installed.

### Flow

1. User installs or builds a `singleton` binary.
2. User runs `singleton install-mcp --client copilot`, `--client claude`, or
   `--client codex`.
3. Singleton builds the client-native registration command.
4. If `--dry-run` is set, singleton prints the command.
5. Otherwise singleton runs the client command and reports success or the native
   command failure.

### Expected behavior

- Generated registrations point at `singleton serve --stdio --backend copilot`
  by default.
- `--binary`, `--backend`, `--database`, and `--name` customize the generated
  MCP server.
- `install-mcp` does not edit client config files directly.

---

## 1. Foreground Agent as Coordinator

### Goal

A user asks their current foreground agent to coordinate multiple background
agent sessions.

### Flow

1. Foreground agent calls `get_capabilities`.
2. Foreground agent decides which tasks can run independently.
3. Foreground agent creates one singleton session per independent task.
4. Foreground agent sends each task with `send_message`.
5. Foreground agent polls `read_events` and `get_inbox`.
6. Foreground agent asks the user about approvals or ambiguous input when
   needed.
7. Foreground agent calls `resolve_request`.
8. Foreground agent summarizes completed background work.
9. Foreground agent closes completed sessions and temporary workspaces with
   `close_resource`.

### Expected behavior

- Background sessions continue running while the foreground agent works on
  other tasks.
- The foreground agent can recover state after context loss with
  `list_sessions`, `get_session`, and event cursors.
- Long-running sessions do not block the MCP call that started them.

---

## 2. Fresh Git Worktree Task

### Goal

Run a background editing task in an isolated repo checkout.

### Flow

1. Foreground agent calls `create_session` with an inline `git_worktree`
   workspace spec.
2. Singleton creates a local worktree from the requested repo/base ref.
3. Singleton creates the backend session with the worktree as default
   workspace.
4. Foreground agent calls `send_message` with the implementation prompt.
5. Singleton persists the turn intent and dispatches it to the backend.
6. Foreground agent calls `read_events` until the turn completes or requires
   input.
7. Foreground agent inspects the completion summary and workspace metadata.
8. Foreground agent decides whether to keep or delete the worktree.
9. Foreground agent calls `close_resource` for the session and, if appropriate,
   the workspace.

### Expected behavior

- The `create_session` response includes both `session_id` and resolved
  `workspace_id`.
- Workspace cleanup follows the workspace cleanup policy or explicit
  `close_resource` request.
- Deleting a workspace fails while active sessions still reference it unless
  forced.

---

## 3. Shared Workspace Research

### Goal

Run several read-only research sessions against one shared checkout.

### Flow

1. Foreground agent calls `ensure_workspace` with a git worktree or local path.
2. Singleton returns a reusable `workspace_id`.
3. Foreground agent calls `create_session` multiple times with
   `existing_workspace`.
4. Foreground agent dispatches research prompts with `send_message`.
5. Foreground agent periodically calls `read_events` for each session cursor.
6. Foreground agent aggregates findings.
7. Foreground agent archives sessions with `close_resource`.
8. Foreground agent keeps or deletes the shared workspace explicitly.

### Expected behavior

- Shared workspace use is explicit.
- Editing tasks should not silently share a workspace unless the foreground
  agent requests it.
- Session closure does not remove the shared workspace by default.

---

## 4. Inbox Fan-In

### Goal

Find all background sessions that need attention.

### Flow

1. Foreground agent calls `get_inbox`.
2. Singleton returns counts and compact actionable items.
3. Foreground agent handles items by kind:
   - permission request: ask the user or apply policy, then call
     `resolve_request`
   - input request: collect an answer, then call `resolve_request`
   - failed turn: inspect session/events, optionally retry, then `ack_inbox`
   - completed turn: read final events, summarize, then `ack_inbox`
   - stale session: inspect or close the session
4. Foreground agent repeats until there are no blocking items.

### Expected behavior

- `get_inbox` is concise enough to call often.
- It does not return large transcripts or diffs.
- Inbox items include ids needed for follow-up tools.
- `ack_inbox` clears handled unread turn items without closing sessions or
  mutating backend transcripts.

---

## 5. Permission or Input Resolution

### Goal

Let background sessions request human or foreground-agent decisions without
owning the foreground conversation.

### Flow

1. Backend emits a permission/input/elicitation request.
2. Singleton stores the request and appends `request.created`.
3. Request appears in `get_inbox`.
4. Foreground agent decides whether it can resolve directly.
5. If needed, foreground agent asks the user in its own UI.
6. Foreground agent calls `resolve_request` with approval, denial, response, or
   cancellation.
7. Singleton records `request.resolved` and forwards the result to the backend.

### Expected behavior

- Requests are durable across daemon restarts.
- Resolution is atomic and idempotent.
- Denials carry a reason when useful.
- Singleton does not invent human approval; the foreground agent or user makes
  that decision.

---

## 6. Resume After Foreground Context Loss

### Goal

A foreground agent loses context or a user starts a new foreground session, but
background sessions continue.

### Flow

1. New foreground agent calls `get_capabilities`.
2. It calls `get_inbox` for actionable state.
3. It calls `list_sessions` for active/recent sessions.
4. It calls `get_session` for sessions it wants to resume coordinating.
5. It uses each session's latest cursor to call `read_events`.
6. It continues coordination from durable singleton state.

### Expected behavior

- No singleton-owned hub transcript is required for recovery.
- Event cursors and summaries provide enough state for a new foreground agent
  to resume orchestration.
- Backend transcripts remain backend-owned.

---

## 7. Cancel a Running Turn

### Goal

Stop a runaway or obsolete background turn.

### Flow

1. Foreground agent sees a running turn through `get_session`, `read_events`, or
   `get_inbox`.
2. Foreground agent calls `cancel_turn`.
3. Singleton records the cancellation request.
4. Singleton forwards cancellation to the backend.
5. Backend confirms cancellation or reports failure.
6. Singleton appends `turn.cancelled` or `turn.failed`.

### Expected behavior

- Cancellation is best-effort when backend support is limited.
- Cancelled turns remain visible in history.
- Session can accept later turns unless backend marks it broken.

---

## 8. Session and Workspace Cleanup

### Goal

Safely close long-lived resources.

### Flow

1. Foreground agent calls `close_resource` for completed sessions.
2. Singleton archives or disposes the backend session according to disposition.
3. Foreground agent calls `close_resource` for disposable workspaces.
4. Singleton checks active references.
5. Singleton deletes paths only when policy and active references allow it.
6. Singleton returns a cleanup summary.

### Expected behavior

- Cleanup calls are idempotent.
- Workspace deletion is never an accidental side effect of session closure.
- Forced deletion is explicit and auditable.

---

## 9. Future Remote Host Flow

### Goal

Run a session on another host while keeping the same MCP control surface.

### Flow

1. Foreground agent calls `get_capabilities` and sees remote host support.
2. Foreground agent calls `ensure_workspace` with a host id and repo/worktree
   spec.
3. Singleton connects to the host through a host connector.
4. Host connector provisions the workspace.
5. Foreground agent creates a session using that workspace.
6. Events, requests, and changesets are normalized into singleton's store.

### Expected behavior

- The MCP tools do not change when host placement changes.
- Host connectors advertise capabilities and limitations.
- Secrets are referenced through host/provider config, not stored raw in
  singleton's SQLite database.

---

## 10. Future AHP Host Connector Flow

### Goal

Federate an external AHP-speaking host into singleton.

### Flow

1. Singleton connects to an AHP host as a client.
2. It subscribes to root/session/chat/terminal/changeset channels.
3. It receives snapshots and ordered action streams.
4. It maps AHP resources to singleton hosts, workspaces, sessions, chats,
   turns, requests, terminals, and changesets.
5. Foreground agents keep using the same singleton MCP tools.

### Expected behavior

- AHP is isolated behind a connector.
- Singleton core does not require AHP crates or protocol types.
- Sequence/replay concepts align with singleton event cursors.
