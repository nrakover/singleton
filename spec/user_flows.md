# singleton - User Flows

## 0. Install and Configure Copilot CLI

### Goal

Install singleton for Copilot CLI without requiring the user to know where
Copilot stores MCP configuration.

### Flow

1. User runs `copilot plugin marketplace add nrakover/singleton`.
2. User runs `copilot plugin install singleton@singleton`.
3. Copilot installs the plugin and loads its `singleton` MCP server definition
   plus the `singleton` foreground-agent Skill.
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

## 0.1 Direct Binary Install and Update

### Goal

Install or update the native `singleton` binary without requiring Rust or manual
release archive extraction.

### Flow

1. User runs
   `curl -fsSL https://github.com/nrakover/singleton/releases/latest/download/install.sh | bash`.
2. The installer resolves the local OS/architecture to a supported release
   target.
3. The installer downloads `singleton-<target>.tar.gz` plus its matching
   `.sha256` file from GitHub Releases.
4. The installer verifies the checksum, extracts the archive, and installs
   `singleton` into `$HOME/.local/bin` or the requested `--install-dir`.
5. The installer prints PATH guidance if the target directory is not currently
   on PATH and points the user at `singleton install-mcp --client copilot`.
6. Later, the user runs `singleton update`.
7. `singleton update` downloads and verifies the matching release asset, compares
   the candidate version with the target binary, and replaces the target through
   a same-directory temporary file and rename.

### Expected behavior

- Users can pin a binary install or update with `--version vX.Y.Z`.
- Users can choose a user-writable target directory with `--install-dir`.
- `--dry-run` shows the selected platform, archive URL, checksum URL, and target
  path without downloading or replacing anything.
- Checksum mismatch, unsupported platforms, missing `curl`/`tar`/checksum tools,
  and unwritable install paths fail explicitly.
- Neither the installer nor `singleton update` invokes `sudo`, edits shell
  startup files, or registers MCP clients by default.
- Updating the binary does not restart a running daemon; singleton tells the
  user to stop/restart the daemon when a running daemon is detected.

---

## 0.2 Direct MCP Client Registration

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

## 0.3 Configure Singleton Defaults

### Goal

Persist singleton preferences for default backend, model, mode, permissions,
hosts, and repo/workspace placement without requiring every foreground agent to
repeat those fields in MCP tool calls.

### Flow

1. User optionally creates
   `${XDG_CONFIG_HOME:-$HOME/.config}/singleton/singleton.toml`.
2. User optionally adds a repo-local `.singleton.toml`.
3. Singleton resolves built-in defaults, user config, project config, env vars,
   and CLI/MCP request fields into one effective config.
4. `singleton serve --stdio` starts or reuses the daemon with that effective
   config.
5. Foreground agent calls `get_capabilities`.
6. Singleton returns configured hosts/backends plus redacted effective defaults.
7. Foreground agent calls `ensure_workspace` or `create_session`, omitting fields
   that match the advertised defaults.

### Expected behavior

- If no config file exists, singleton behaves as if this default config existed:

  ```toml
  version = 1
  default_profile = "default"

  [profiles.default]
  backend = "copilot"
  mode = "interactive"
  state_dir = "~/.singleton"
  database = "~/.singleton/singleton.db"
  default_host = "host_local"
  repo_workspace_provider = "git_worktree"
  cleanup_policy = "keep"

  [profiles.default.permissions]
  default = "ask"

  [hosts.host_local]
  kind = "local"
  ```

- Config file path defaults to `~/.config/singleton/singleton.toml` on
  macOS/Linux.
- Daemon state defaults to `~/.singleton`.
- `mode` controls backend/agent behavior, while `permissions.default` controls
  singleton-managed permission/input requests.
- `repo_workspace_provider = "git_worktree"` applies only to repo-backed
  workspace creation. Non-git directories fall back to `local_path`.
- MCP tool definitions and runtime behavior use the same effective defaults.
- Raw secrets are not stored in SQLite and should not be placed in singleton
  config.

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
8. Foreground agent calls `get_latest_output` for completed or failed turns and
   summarizes the compact result.
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
7. Foreground agent calls `get_latest_output` for the completed turn.
8. If `needs_event_inspection` is true, the foreground agent reads raw turn
   events with `read_events`.
9. Foreground agent inspects the compact result and workspace metadata.
10. Foreground agent decides whether to keep or delete the worktree.
11. Foreground agent calls `close_resource` for the session and, if appropriate,
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
   - failed turn: call `get_latest_output`, inspect raw events only if needed,
     optionally retry, then `ack_inbox`
   - completed turn: call `get_latest_output`, summarize, then `ack_inbox`
   - stale session: inspect or close the session
4. Foreground agent repeats until there are no blocking items.

### Expected behavior

- `get_inbox` is concise enough to call often.
- It does not return large transcripts or diffs.
- Inbox items include ids needed for follow-up tools.
- `ack_inbox` clears handled unread turn items without closing sessions or
  mutating backend transcripts.

---

## 4.1 Latest Output Retrieval

### Goal

Retrieve the latest useful result for a background turn without scanning raw
normalized backend event payloads.

### Flow

1. Foreground agent sees a completed, failed, or cancelled turn through
   `read_events`, `get_inbox`, or `get_session`.
2. Foreground agent calls `get_latest_output` with `session_id` and optionally
   `turn_id`.
3. If `result_text` is present, the foreground agent uses it as the compact turn
   result.
4. If `needs_event_inspection` is true, the foreground agent calls
   `read_events` with the returned `turn_resource_uri` to inspect raw events.
5. After handling an unread completed or failed turn, the foreground agent calls
   `ack_inbox`.

### Expected behavior

- Omitting `turn_id` selects the latest completed, failed, or cancelled turn for
  that session.
- Sessions with no terminal turn return a typed empty result, not an error.
- Unknown Copilot SDK payload shapes produce `needs_event_inspection: true`
  rather than invented result text.
- `read_events` cursor semantics remain monotonic; latest-output retrieval does
  not use negative cursors or mutate event read state.

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
6. It calls `get_latest_output` for unread completed or failed turns.
7. It continues coordination from durable singleton state.

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

## 9. SSH Remote Host Flow

### Goal

Run a session on another host while keeping the same MCP control surface.

### Flow

1. User declares an SSH host in singleton config, for example:

   ```toml
   [hosts.devbox]
   kind = "ssh"
   target = "devbox"
   connect_command = "singleton serve --stdio"
   ```

2. Local singleton starts cached SSH warmup in the background after startup.
3. Foreground agent calls `get_capabilities` and sees the configured host as
   `not_checked`, `checking`, `ready`, or `unavailable` based on cached health.
4. Foreground agent calls `ensure_workspace`/`create_session` with a host id, or
   a default host resolves to the SSH host.
5. Singleton connects to the host by running
   `ssh devbox singleton serve --stdio` unless config overrides the remote
   command.
6. Remote singleton owns its own config/state paths and provisions the
   workspace.
7. Local singleton stores local ids and local-to-remote resource links.
8. Foreground agent sends messages through the local tool surface.
9. Session-targeted `read_events` reconciles from the stored remote cursor and
   mirrors unseen remote events into singleton's local store.

### Expected behavior

- The MCP tools do not change when host placement changes.
- Host connectors advertise capabilities and limitations.
- Normal SSH user, port, key, and proxy settings are delegated to the user's
  central SSH config.
- Secrets are referenced through host/provider config, not stored raw in
  singleton's SQLite database or copied into plaintext singleton config.
- SSH `target` is passed as the exact target/alias; user, port, identity, and
  proxy details live in `~/.ssh/config`.
- Singleton config does not store remote daemon state directories or socket
  paths.
- Project-scoped config cannot silently introduce a non-default remote
  `connect_command` or free-form local `ssh_args`, including by inheriting those
  fields from trusted user config for a project-touched host id.
- Local config does not contain remote singleton state paths; remote state is
  owned by the remote singleton instance.
- The v1 transport opens SSH/MCP per operation. Reusable per-host connections,
  explicit status refresh/doctor commands, and stronger ambiguous-operation
  recovery remain follow-up work.

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
