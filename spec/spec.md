# singleton - Behavioral Specification

## 1. Overview

`singleton` is a durable background agent session broker. It lets a
foreground agent create, message, observe, and close many background agent
sessions through a compact MCP tool surface.

The old "hub" model is now a usage convention, not a product primitive. A
foreground agent can act as a hub by using singleton's MCP tools, but
singleton does not own or render that foreground session.

The first implementation target is a Rust daemon backed by the GitHub Copilot
SDK. The design keeps backend and host seams explicit so future integrations
can add SSH hosts, cloud sandboxes, other agent runtimes, and Agent Host
Protocol (AHP) federation without changing the core concepts.

---

## 2. Core Concepts

### 2.1 Broker daemon

The `singletond` daemon is the local control plane. It:

- serves the MCP tools used by foreground agents
- tracks hosts, workspaces, sessions, turns, requests, and events
- starts and resumes background sessions through backend adapters
- brokers permissions and other input requests
- indexes backend events for fan-in, polling, summaries, and auditability
- manages local workspace lifecycle, including git worktrees

The daemon does not own a user-facing hub session or terminal UI.
Foreground MCP clients should connect through `singleton serve --stdio`, which
acts as a stdio proxy to the daemon's local Unix socket. The proxy may exit when
the foreground agent disconnects; the daemon and its background turn tasks keep
running until `singleton stop` or process termination.

### 2.1.1 Installation and client registration

The primary Copilot CLI installation path is a Copilot plugin in this
repository. Installing the plugin configures a `singleton` MCP server without
requiring users to locate or edit Copilot's MCP JSON files and installs a
`singleton` Skill containing the foreground-agent coordination cookbook:

```bash
copilot plugin marketplace add nrakover/singleton
copilot plugin install singleton@singleton
```

The plugin launcher must:

- keep stdout reserved for MCP JSON-RPC once the foreground client starts it
- write bootstrap diagnostics to stderr
- use `${COPILOT_PLUGIN_DATA}` as persistent writable storage
- install or reuse a released `singleton` binary for the current platform
- exec `singleton serve --stdio --backend copilot`
- declare the plugin-packaged `skills/` directory containing the `singleton`
  foreground-agent Skill

Supported launcher overrides include `SINGLETON_BINARY`,
`SINGLETON_VERSION`, `SINGLETON_RELEASE_BASE_URL`,
`SINGLETON_FORCE_INSTALL`, `SINGLETON_BACKEND`, and `SINGLETON_DATABASE`.

Direct CLI registration remains available for Copilot CLI, Claude Code, Codex,
and other stdio MCP clients:

```bash
singleton install-mcp --client copilot
singleton install-mcp --client claude
singleton install-mcp --client codex
```

`singleton mcp-config` remains the manual JSON escape hatch.

### 2.2 Foreground agent

A foreground agent is any MCP-capable agent currently interacting with the
user. It discovers singleton's tools, creates background sessions, polls for
events, resolves requests, and summarizes results.

Copilot foreground agents can load the plugin-packaged `singleton` Skill for
the recommended coordination cookbook.

This is the new hub convention:

1. The user asks the foreground agent to coordinate work.
2. The foreground agent creates singleton sessions for independent tasks.
3. The foreground agent uses `get_inbox` and `read_events` to fan in state.
4. The foreground agent resolves permission/input requests.
5. The foreground agent reports outcomes to the user.

### 2.3 Host

A host is a compute/control endpoint where workspaces and sessions can live.
Examples:

- local machine
- SSH target
- cloud sandbox
- future external AHP host

The MVP ships only the local host connector, but host identity, capabilities,
and connection metadata are first-class from the start.

### 2.4 Workspace

A workspace is a filesystem or repository context on a host. It is not an
agent conversation.

Workspace metadata includes:

- host id
- local or remote path
- repository identity
- git worktree path
- branch and base ref
- cleanup policy
- current status

Sessions use workspaces. A workspace may be shared by multiple sessions or
reserved for one session, depending on policy. Editing sessions should default
to isolated git worktree workspaces.

### 2.5 Session, chat, and turn

A session is a durable logical agent conversation or task container. It records
the selected backend, backend session id, model/mode/config, default
workspace, lifecycle state, and event cursors.

A chat is a conversation stream within a session. The MVP may use one default
chat per session, but the data model reserves chat identity so future AHP
mapping and subagent/team patterns stay clean.

A turn is one foreground request sent into a session/chat. Turns are
asynchronous by default: `send_message` returns a `turn_id`, and the foreground
agent observes progress through `read_events` or `get_inbox`.
The broker persists the turn before dispatching it to the backend and then
continues the backend call in a Tokio background task.

---

## 3. State Ownership Boundary

`singleton` is not a replacement for the agent backend's persistence layer.
It stores orchestration state and projections, not a second canonical copy of
the backend conversation.

### 3.1 Owned by singleton

- stable singleton ids and resource URIs
- host, workspace, session, chat, turn, request, and changeset catalogues
- mapping from singleton ids to backend-native ids
- workspace lifecycle and git worktree metadata
- foreground-agent control state: queued turns, active turns, pending requests,
  resolved requests, cancellation, archival, and cleanup
- normalized event index with sequence numbers and cursors
- derived summaries, activity, inbox items, and changeset metadata
- singleton-authored facts such as "workspace created" or "approval denied"

### 3.2 Owned by the backend

For the MVP backend, the GitHub Copilot SDK owns:

- canonical conversation transcript and model context
- backend session persistence and resume integrity
- backend message/tool-call ids and runtime state
- model/tool planning and execution semantics
- provider-specific durability and consistency guarantees
- backend-managed cloud lifecycle when cloud sessions are used later

### 3.3 Owned by the workspace/filesystem

The filesystem owns:

- source files
- generated files
- git index and commits
- build outputs
- untracked files

Singleton tracks where these files live and may index changes, but it does not
mirror the full filesystem into SQLite.

### 3.4 Boundary rules

- Persist singleton intents before calling the backend, then reconcile with
  backend acknowledgements and events.
- Store backend ids and resume metadata, not a second transcript.
- Treat backend event streams as the source of truth for conversation content.
- Treat singleton state as source of truth for workspace ownership, cleanup
  policy, labels, and approval records.
- If backend state disappears, mark the session degraded or broken.
- If singleton state disappears, recovery is best-effort unless backend ids
  were exported or rediscovered.

---

## 4. MCP Tool Surface

The default MCP surface should stay small to avoid foreground-agent context
pollution. Advanced capabilities can be added later, but the MVP tools should
cover orchestration without requiring the human user to manage sessions
manually.

Default tools:

| Tool | Purpose |
|---|---|
| `get_capabilities` | Return available hosts, backends, workspace strategies, limits, and protocol versions. |
| `get_inbox` | Return a compact fan-in view of pending permissions, input requests, failed turns, unread completions, and stale sessions. |
| `ack_inbox` | Mark unread completed or failed turn inbox items as read after the foreground agent has handled them. |
| `ensure_workspace` | Create or resolve a workspace, including local paths and git worktrees. |
| `create_session` | Create a background session, optionally with an inline workspace spec. |
| `send_message` | Start an asynchronous turn and return a `turn_id`. |
| `read_events` | Read or long-poll sequence-numbered events for a resource. |
| `list_sessions` | List active/recent sessions for resuming coordination. |
| `get_session` | Inspect one session, including workspace summary and cursors. |
| `resolve_request` | Resolve permission, input, or other pending requests. |
| `cancel_turn` | Cancel an active turn. |
| `close_resource` | Archive, dispose, or delete sessions and workspaces according to policy. |

Tools intentionally collapsed out of the default surface:

- `wait_for_event` is `read_events(wait_ms=...)`.
- `read_output` is deferred until event payloads become too large.
- `list_pending_approvals` is covered by `get_inbox`.
- separate approve/deny tools are `resolve_request(decision=...)`.
- completion/read-state mutations are `ack_inbox`.
- separate archive/delete tools are `close_resource(disposition=...)`.
- separate host/backend list tools are `get_capabilities`.

---

## 5. Workspace Behavior

Workspaces can be created explicitly with `ensure_workspace` or inline during
`create_session`.

Supported MVP workspace specs:

- existing workspace id
- local path
- local git worktree from a repo path or URL
- backend default workspace

Rules:

- `create_session` with an inline workspace spec must return the resolved
  `workspace_id`.
- destructive workspace cleanup must be explicit through `close_resource` or
  a cleanup policy chosen when the workspace is created.
- closing a session does not delete its workspace unless policy permits it.
- deleting a workspace with active sessions must fail unless `force=true`.
- cleanup operations must be idempotent.

---

## 6. Backend and Host Architecture

### 6.1 Agent backend

The MVP backend is GitHub Copilot SDK for Rust. It should be wrapped behind an
`AgentBackend` trait so later backends can be added without changing the MCP
contract.

The MVP must validate:

- create session
- resume session
- send message
- subscribe to events and normalize SDK events into singleton events
- cancel/abort a turn
- permission, elicitation, and input handlers backed by durable singleton
  requests resolved through `resolve_request`
- model/mode configuration
- custom tools or MCP integration

The current `AgentBackend` contract accepts a fallible event sink during
`send_message`. Backends emit normalized `BackendEvent` values into that sink
while the broker-owned background task remains responsible for terminal turn
status reconciliation.

Backends also advertise whether they can resume sessions and whether they can
reattach already-running turns after broker restart. On startup, the broker
resumes persisted backend sessions when supported. Active turns are reattached
only when the backend explicitly supports turn reattach; otherwise they are
marked failed/unread with a retryable interruption event and pending requests
for that turn are cancelled. The Copilot MVP resumes sessions for future turns,
but does not claim active-turn reattach.

### 6.2 Host connector

Host placement is separate from agent backend choice. The MVP implements
`LocalHostConnector`. Future connectors include:

- SSH remote runner
- cloud sandbox provider
- AHP host connector

The host interface must include stable host ids, capabilities, auth reference
metadata, and optional future AHP endpoint metadata from the start.

### 6.3 AHP relationship

AHP is an alignment target, not an MVP dependency.

The most important future role is `singletond` as an AHP client: it can connect
to an external AHP host, subscribe to that host's root/session/chat/terminal
and changeset channels, and normalize those events into singleton state.

A downstream AHP server/control surface for editors or dashboards is possible
later, but lower priority than the AHP host connector.

Singleton's internal resources should be mappable to AHP concepts:

- root: broker capabilities and catalogues
- host: compute endpoint
- workspace: filesystem/repo context
- session: durable agent container
- chat: conversation stream
- turn: user request and backend response lifecycle
- request: permission/input/elicitation item
- changeset: diff or file-change view
- terminal: long-running process surface

---

## 7. Durable State Model

Use SQLite for MVP state, with append-heavy event storage and explicit
migrations.

Current tables:

- `hosts`
- `workspaces`
- `sessions`
- `chats`
- `turns`
- `requests`
- `events`
- `resource_states`
- `changesets`
- `artifacts`

Daemon lifecycle state is intentionally filesystem-backed: the default SQLite
database is `~/.singleton/singleton.db`, with sibling pid/socket files. An
explicit `--database` path derives sibling pid/socket paths, with long socket
paths hashed into the system temp directory to satisfy Unix socket limits.

`events` must include:

- `event_id`
- `server_seq`
- `resource_uri`
- `parent_resource_uri`
- `event_type`
- `origin_kind`
- `origin_id`
- `payload_json`
- `created_at`

This supports MCP polling immediately and future AHP-like replay/snapshot
adapters later.

`read_events(session_id=...)` must include events whose `resource_uri` is the
session as well as child events whose `parent_resource_uri` is the session. This
lets foreground agents poll one session cursor and still receive turn/request
events.

---

## 8. Rust Toolchain

The implementation target is Rust; the superseded Python/uv prototype has been
removed from the active repository surface.

Baseline:

- Rust 1.94.0 or newer
- Rust 2024 edition
- `rust-toolchain.toml` pinned to the selected stable toolchain
- Cargo workspace layout

Likely crates:

- `singleton-core`
- `singleton-store`
- `singleton-copilot`
- `singleton-mcp`
- `singleton-host`
- `singleton-cli`
- `singleton-test-support`

Core validation commands:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets
```

Live Copilot-backed tests should be ignored by default and run explicitly:

```bash
cargo test --workspace --features live-copilot -- --ignored
```

---

## 9. Non-Goals for MVP

- no daemon-owned hub TUI
- no Python implementation work beyond preserving historical reference until
  Rust replacement is ready
- no second real agent backend
- no custom transcript/context manager
- no custom model planner
- no full filesystem mirror
- no SSH/cloud host implementation
- no runtime dependency on AHP
- no generic scheduler beyond session dispatch, cancellation, requests, and
  event fan-in
