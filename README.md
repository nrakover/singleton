# singleton

`singleton` is being redesigned as a durable background agent session broker.
It will expose a compact MCP server that any capable foreground agent can use
to create, message, monitor, and clean up long-lived background agent sessions.

The previous Python/Claude hub implementation in this repository is now
historical reference. Current design work targets a Rust daemon backed first by
the GitHub Copilot SDK.

## Product direction

The foreground agent you are already using becomes the "hub" by convention: it
calls singleton MCP tools to coordinate background sessions, handle approvals,
fan in events, and summarize results. Singleton provides the durable control
plane, not a foreground chat UI.

Core concepts:

- **Host**: compute/control endpoint such as local machine, SSH target, cloud
  sandbox, or future AHP host.
- **Workspace**: filesystem or repository context on a host, including local
  paths and git worktrees.
- **Session**: durable logical agent conversation/task container using an
  agent backend and a default workspace.
- **Turn**: one asynchronous request sent to a session.
- **Inbox**: compact fan-in view of permission requests, input prompts, failed
  turns, unread completions, and stale sessions.

## MVP target

- Rust `singletond` daemon
- MCP control surface
- SQLite durable store
- local host connector
- local git worktree workspace provider
- GitHub Copilot SDK backend
- deterministic fake backend for tests
- CLI admin commands: `serve`, `status`, `stop`

Default MCP tools:

- `get_capabilities`
- `get_inbox`
- `ensure_workspace`
- `create_session`
- `send_message`
- `read_events`
- `list_sessions`
- `get_session`
- `resolve_request`
- `cancel_turn`
- `close_resource`

## Architecture notes

Singleton owns orchestration state, workspace lifecycle, request/approval
state, and a normalized event index. The agent backend owns canonical
conversation persistence and model runtime state. The workspace/filesystem owns
source files, git index, commits, generated files, and untracked files.

AHP is an alignment target and future adapter surface. The near-term AHP role
is singleton as a client/connector to external AHP hosts; AHP is not an MVP
runtime dependency.

## Development status

The repository now contains the first Rust broker slice: a Cargo workspace,
SQLite store, local host/worktree connector, fake backend, rmcp-backed MCP tool
surface, Copilot SDK adapter, and thin CLI. See:

- `spec/spec.md`
- `spec/interfaces.md`
- `spec/user_flows.md`
- `spec/tests.md`
- `project_tasks/2_agent-session-mcp-pivot.md`
- `docs/foreground-agent-coordination.md`
- `AGENTS.md`

The old Python code, tests, hook scripts, and slash-command docs remain for
reference until the Rust replacement is implemented.

## CLI smoke usage

```bash
cargo +1.94.0 run -p singleton-cli --bin singleton -- serve --once
cargo +1.94.0 run -p singleton-cli --bin singleton -- serve --stdio
cargo +1.94.0 run -p singleton-cli --bin singleton -- status
```

`serve --stdio` exposes the default singleton MCP tools over stdio using
`rmcp`.

## Planned verification

Rust implementation work should pass:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets
```

Live Copilot-backed tests should be ignored by default and run explicitly:

```bash
cargo test --workspace --features live-copilot -- --ignored
```
