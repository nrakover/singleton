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

Completed P0 daemon lifecycle hardening:

- Auto-started daemons are spawned into their own Unix process group via Rust's
  safe standard-library process-group API.
- Daemon startup is serialized per state database with a lock file around stale
  socket cleanup, bind, and pid writes.
- `singleton status` reports running, stopped, stale pid, stale socket, combined
  stale pid/socket, and degraded states with cleanup guidance.
- `singleton start` and `singleton serve --stdio` are idempotent when the daemon
  is already running; `singleton serve --daemon` fails clearly if another daemon
  owns the state database.

Completed first config execution slice:

- Added a typed singleton TOML config loader with default synthesis, redaction,
  validation, and precedence tests for user/project/explicit config, env vars,
  CLI backend/database overrides, `host_local`, SSH connect-command safety, and
  repo-source provider fallback.
- Threaded effective backend/database/profile/default metadata through CLI
  daemon commands, generated MCP registrations, broker capability defaults, and
  backend selection.

Remaining MCP and daemon usability follow-ups:

- Fill broker/MCP request defaults from `EffectiveConfig`: default host, mode,
  permissions, cleanup policy, and repo workspace provider.
- Render MCP tool schema defaults from the same effective config when the schema
  layer can do so truthfully; omit misleading static defaults until then.
- Extend `get_latest_output` extraction fixtures as more Copilot SDK event
  payload shapes are recorded; keep unknown shapes behind
  `needs_event_inspection` rather than guessing result text.
- Add a cookbook prompt/config example showing the intended Copilot CLI flow:
  `get_capabilities`, `create_session`, `send_message`, `read_events`,
  `get_latest_output`, and `ack_inbox`.
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

- Completed SSH v1 foundation: trusted descriptors, MCP-over-SSH stdio calls,
  cached host health, local/remote id mapping, mirrored event replay, remote
  request/cancel/cleanup forwarding, and fake remote broker tests.
- Keep capability/UX gating conservative: configured SSH hosts start as
  `not_checked` and only advertise remote providers/backends after a successful
  cached remote capability handshake.
- Follow up with reusable per-host SSH/MCP connections, explicit
  `status --refresh`/doctor probing, stronger ambiguous-operation recovery after
  crashes, and ignored real SSH smoke tests.
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
