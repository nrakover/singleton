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

## MCP and daemon usability

- Detach the auto-started daemon from the foreground MCP proxy process group so
  exiting Copilot CLI does not kill the daemon when the MCP child process tree is
  torn down.
- Add an interprocess daemon startup lock around socket cleanup/bind/pid writes
  so concurrent foreground MCP proxies cannot race while auto-starting the same
  daemon.
- Teach `singleton status` to report stale pid/socket files explicitly, and
  either clean them automatically or print the exact `singleton stop` cleanup
  command.
- Document command semantics: `singleton start` and `singleton serve --stdio`
  should be idempotent when the daemon is already running, while
  `singleton serve --daemon` should fail clearly if another daemon owns the
  state database.
- Add a compact `get_latest_output` or summarized turn-result field so
  foreground agents do not need to inspect large raw SDK event payloads to find
  the final assistant answer.
- Evaluate whether "latest output" should be a dedicated tool, a `get_session`
  summary field, or a `read_events` tail mode. Avoid overloading ordinary
  monotonic cursor semantics unless negative cursor behavior is explicitly
  specified.
- Add namespace support for multi-foreground-agent scenarios: default to a
  `default` namespace, allow foreground agents to specify a namespace on tools,
  and scope session lists, inboxes, request resolution, and cleanup by namespace.
- Decide whether inbox read state should be global or per-foreground-client
  before multi-operator workflows become common.

## Packaging and installation

- Publish `singleton-cli` to crates.io once the binary name/package layout is
  stable enough for `cargo install singleton`.
- Add Homebrew distribution for macOS users who do not want to use Cargo or the
  Copilot plugin bootstrapper.
- Add Windows release artifacts and plugin launcher support after the daemon
  transport story has a Windows equivalent to Unix sockets.
- Decide whether to add `singleton self-update` or keep updates delegated to
  Copilot plugin updates, Homebrew, Cargo, and GitHub Releases.

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
