# Project backlog

Purpose: curated follow-up work for the Rust MCP session broker pivot.

---

## Rust foundation decisions

- Choose SQLite crate: `sqlx` vs `rusqlite`.
- Choose Rust MCP server crate.
- Validate exact GitHub Copilot SDK APIs for create, resume, send, cancel,
  event streaming, permissions, and live/cloud capabilities.

## Broker MVP

- Add Cargo workspace skeleton and pinned Rust toolchain.
- Implement fake backend/host test support.
- Implement SQLite migrations and repositories.
- Implement default MCP tools.
- Implement local git worktree workspace lifecycle.
- Implement Copilot SDK adapter.

## Foreground-agent hub convention

- Write coordinator prompt snippets for MCP-capable foreground agents.
- Add examples for parallel research, isolated worktree tasks, inbox fan-in,
  request resolution, cancellation, and cleanup.

## Remote hosts

- Extend the initial `RemoteRunner`/`SshHostConnector` scaffold into a full
  remote session runner with reconnect/replay semantics.
- Evaluate cloud sandbox providers such as GitHub-hosted sessions or Daytona.
- Support repo-homed workspaces on remote hosts.

## AHP integration

- Track AHP protocol stability.
- Prototype `AhpHostConnector` with singleton as an AHP client.
- Later evaluate an optional downstream AHP control surface for dashboards or
  editors.

## Optional human inspection surfaces

- Consider `singleton attach <session_id>` for debug inspection.
- Consider a lightweight dashboard only after MCP broker flows are proven.
