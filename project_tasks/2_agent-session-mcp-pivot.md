# Task 2: Agent Session MCP Pivot

## Objective

Reframe `singleton` as a Rust-based MCP session broker that any capable
foreground agent can use to dispatch and manage durable background agent
sessions.

This task supersedes both prior implementation plans:

- `project_tasks/0_bootstrap.md`
- `project_tasks/1_streamed-architecture-reset.md`

The old Python/Claude hub implementation remains useful as historical
reference, but new implementation work should target the Rust MCP broker
specified in `spec/`.

---

## Product Model

`singleton` is a durable background agent session broker.

It provides:

- a compact MCP server for foreground agents
- durable host/workspace/session/turn/request/event state
- asynchronous background turn dispatch and event polling
- permission/input request brokering
- first-class local git worktree workspaces
- a Copilot SDK backend for the MVP
- explicit host/backend seams for SSH, cloud sandboxes, and future AHP hosts

A "hub" is no longer a singleton-owned foreground process. It is a convention:
the user's current foreground agent acts as coordinator by using singleton MCP
tools.

---

## Current Scope

### MVP

- Rust daemon: `singletond`
- MCP control surface
- SQLite durable store
- local host connector
- local git worktree workspace provider
- GitHub Copilot SDK backend
- deterministic fake backend for tests
- CLI admin commands: `serve`, `start`, `status`, `stop`, `mcp-config`
- sequence-numbered event stream
- fan-in inbox
- safe/idempotent cleanup

### Explicitly out of scope for MVP

- second real agent backend
- daemon-owned hub TUI
- Python hub/worker extension work
- SSH/cloud execution
- runtime AHP dependency
- full transcript/context manager
- full filesystem mirror

---

## Architecture Decisions

### Control surface

The default MCP profile exposes only:

- `get_capabilities`
- `get_inbox`
- `ack_inbox`
- `ensure_workspace`
- `create_session`
- `send_message`
- `read_events`
- `list_sessions`
- `get_session`
- `resolve_request`
- `cancel_turn`
- `close_resource`

Advanced/admin capabilities should be kept out of the default profile unless
foreground agents demonstrably need them.

### Resource model

Keep separate resources for:

- host
- workspace
- session
- chat
- turn
- request
- changeset
- terminal
- artifact

Store stable resource URIs and monotonic event sequence numbers from day one.

### State ownership

Singleton owns orchestration state, workspace lifecycle, approval state, and an
event index. The Copilot SDK owns canonical conversation persistence and model
runtime state. The filesystem owns source files, git index, commits, build
artifacts, and untracked files.

### AHP alignment

AHP is an alignment target and future adapter surface, not an MVP dependency.
The first important AHP role is `singletond` as a client/connector to external
AHP hosts. A downstream AHP server surface may be evaluated later.

---

## Proposed Rust Workspace

Current crate layout:

- `crates/singleton-core`
- `crates/singleton-store`
- `crates/singleton-copilot`
- `crates/singleton-mcp`
- `crates/singleton-host`
- `crates/singleton-broker`
- `crates/singleton-cli`
- `crates/singleton-test-support`

Toolchain:

- Rust 1.94.0 or newer
- Rust 2024 edition
- pinned `rust-toolchain.toml`

Verification gate:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets
```

Live Copilot gate:

```bash
cargo test --workspace --features live-copilot -- --ignored
```

---

## Implementation Phases

### Phase 0: design reset

- rewrite `spec/spec.md`
- rewrite `spec/interfaces.md`
- rewrite `spec/user_flows.md`
- rewrite `spec/tests.md`
- update project task docs and backlog
- update developer guidance to identify the Rust broker as the current target

### Phase 1: Rust foundation spike

- add Cargo workspace skeleton
- validate GitHub Copilot SDK APIs
- choose SQLite crate
- choose Rust MCP server crate
- implement fake backend and fake host test support
- prove the resource/event model can project to AHP-like snapshots without
  importing AHP runtime types

### Phase 2: broker MVP

- implement migrations and repositories
- implement broker service and event appender
- implement local host/workspace provider
- implement MCP tools
- implement Copilot adapter vertical slice
- implement CLI admin commands

Current Phase 2 status:

- `send_message` now persists turns and returns while backend dispatch continues
  in a Tokio background task.
- `AgentBackend::send_message` accepts a fallible broker event sink so fake and
  Copilot backends can emit normalized events while a turn is running.
- The Copilot adapter now supports real SDK event subscription ingestion and
  store-backed permission, input, and elicitation handlers.
- `resolve_request` stores both decision and response so backend handlers can
  map singleton resolutions back to provider-specific responses.
- `singleton serve --backend copilot --stdio` selects the real Copilot backend;
  fake remains available for deterministic tests and local smoke checks.
- Broker startup reconciles persisted queued/running turns as interrupted so a
  restarted local broker does not leave stale active turns.
- CLI stdio integration test covers MCP initialize, tool discovery, create
  session, async send, and event polling through the fake backend.
- `singleton serve --stdio` now starts/reuses a daemon and proxies stdio to a
  Unix socket, so foreground MCP client disconnects do not kill broker-owned
  turns. `serve --stdio --direct` remains available for foreground debugging.
- `singleton start`, `singleton status`, and `singleton stop` manage daemon
  pid/socket lifecycle for a selected state database.
- `singleton mcp-config --backend copilot` prints an MCP server config snippet
  using the daemon-backed stdio entrypoint.
- Broker startup now resumes persisted backend sessions when supported and only
  preserves active turns when the backend also supports active-turn reattach.
  Otherwise active turns are failed/unread with retryable interruption events.
- Cancelling or interrupting a turn now cancels pending requests for that turn so
  backend permission/input/elicitation handlers can unblock.
- `ack_inbox` marks unread completed/failed turn inbox items as read.
- Ignored live Copilot smoke covers real SDK session creation plus send/event
  completion when run with `--features live-copilot -- --ignored`.
- Ignored live CLI smoke covers `singleton serve --backend copilot --stdio`
  through MCP initialize/create/send/read-events.

### Phase 3: hub convention docs

- document coordinator prompts for MCP-capable foreground agents
- add examples for parallel research, isolated worktree tasks, inbox handling,
  approvals, cancellation, and cleanup

Current artifact: `docs/foreground-agent-coordination.md`.

### Phase 4: remote/backend fast follow

- define host runner protocol
- add SSH host support
- evaluate cloud sandbox providers
- evaluate `AhpHostConnector` once AHP stabilizes enough for integration

Current artifacts:

- `singleton-host::RemoteRunner`
- `singleton-host::SshHostConnector`
- `docs/remote-host-fast-follow.md`

---

## Completion Checklist

- specs describe the Rust MCP broker model consistently
- old plans are clearly marked superseded
- Rust workspace skeleton exists
- default MCP tools have contract tests
- fake backend supports deterministic end-to-end tests
- local git worktree lifecycle is tested
- Copilot adapter has opt-in live tests
- CLI can select fake or Copilot backend
- CLI stdio MCP integration test exists
- CLI daemon start/status/stop integration test exists
- MCP inbox acknowledgement tool exists
- broker restart reattach/interruption behavior is tested
- turn cancellation cancels pending requests
- live Copilot stdio MCP integration test exists
- full Rust validation gate passes
