# Remote Host Fast Follow

Remote execution is not modeled as the local broker running ad hoc shell
commands on another machine. SSH host support means local singleton brokering to
a remote singleton control surface over SSH stdio.

## Product model

An SSH host descriptor names a trusted remote singleton endpoint:

```toml
[hosts.devbox]
kind = "ssh"
target = "devbox"
connect_command = "singleton serve --stdio"
ssh_args = ["-T", "-o", "BatchMode=yes"]
```

`target` is the exact SSH target or alias. Normal SSH user, port, key, and proxy
settings belong in the user's central SSH config. `connect_command` defaults to
`singleton serve --stdio` and is the remote command used to start or reach the
remote singleton MCP control surface.

Do not add `remote_state_dir` to local SSH host config. Remote singleton state,
workspace allocation, sessions, turns, requests, backend ids, and cleanup are
owned by the remote singleton instance.

## State ownership

- **Remote singleton** is canonical for remote workspaces, sessions, turns,
  requests, backend ids, backend resume semantics, and workspace cleanup.
- **Local singleton** is canonical for configured host descriptors, local
  resource ids, local-to-remote resource links, forwarded-operation state,
  mirrored event cursors, local inbox/read state, and connection health.
- **The backend** remains canonical for transcripts and model/runtime state.

The foreground MCP tool surface should stay stable. Remote placement changes
where local singleton routes the operation, not which tool the foreground agent
calls.

## Implemented v1 slice

The current SSH slice supports:

- trusted SSH descriptors from effective config;
- cached host health persisted in SQLite;
- startup warmup scheduled in the background after broker construction;
- MCP initialize plus `tools/call` over `ssh ... <target> <connect_command>`;
- host routing for remote session placement;
- remote workspace/session creation with local-to-remote resource links;
- remote turn forwarding;
- session-targeted event reads that call remote `read_events` from the stored
  remote cursor and mirror unseen events locally;
- compact latest output derived from mirrored local events;
- request resolution, turn cancellation, and resource close forwarding when the
  local resource has a remote link; and
- `singleton status` showing configured SSH hosts from cached state without
  probing.

The transport currently opens a fresh SSH/MCP process per remote operation. That
keeps the correctness surface small for v1, but a reusable per-host connection
manager remains a follow-up.

## Connection lifecycle

On daemon startup/restart, singleton starts warmup attempts for configured SSH
hosts in the background. Warmup performs MCP initialize and `get_capabilities`.
It must not create workspaces or sessions.

Host health must distinguish:

- `not_checked`: host was configured but no attempt has been scheduled yet
- `checking`: startup warmup or refresh is in progress
- `available`: compatible remote singleton and usable backend were seen
- `unavailable`: SSH/MCP failed after a real attempt
- `incompatible`: remote singleton lacks required protocol/capabilities
- `degraded`: mapped resources exist but reconciliation is incomplete
- `backoff`: automatic reconnect is delayed after repeated failures

`get_capabilities` reports cached health and never marks a fresh host
unavailable merely because no connection has been attempted. Session-targeted
event and latest-output reads attempt bounded reconciliation before returning
local mirror data. Broad fan-in reads do not block on every remote host.

## Reconnect and reconciliation

Reconnect is driven by startup warmup, active remote work, pending forwarded
operations, host-targeted reads/writes, explicit refresh/doctor commands, and
fan-in noticing stale active remote sessions.

A full reusable-connection reconnect attempt should:

1. Open SSH and complete MCP initialize.
2. Verify protocol compatibility, federation/idempotency capabilities, remote
   backend usability, and stable remote broker identity.
3. Reconcile pending forwarded operations by retrying the same idempotency key or
   querying operation status.
4. For each mapped active/degraded remote session, call remote `get_session` and
   `read_events` from the last stored remote cursor.
5. Append only unseen remote events to local event storage, preserving remote
   cursor metadata while assigning local `server_seq` values.
6. Update local workspace/session/turn/request state from mirrored remote events
   and remote acknowledgements.

If the remote identity changed, a mapped resource is unknown, or the remote
cursor can no longer be replayed, mark the local host/session mapping degraded
instead of guessing or issuing a second non-idempotent operation.

## Protocol direction

Use MCP-over-SSH for the first remote SSH implementation, but hide it behind a
remote broker client abstraction.

MCP is the v1 choice because it reuses the existing singleton contract, keeps the
manual `ssh devbox singleton serve --stdio` escape hatch natural, and lets fake
remote MCP tests validate both foreground and broker-to-broker behavior.

A bespoke peer/pub-sub protocol may become worthwhile later if MCP forwarding
forces awkward semantics around idempotency, request-policy delegation, event
stream efficiency, reconnect correctness, or compatibility negotiation. Until
then, do not create a second protocol surface.

## Implementation stack

Completed:

1. SSH config descriptors and trust rules.
2. Federation/idempotency metadata and local-to-remote resource links.
3. Remote broker registry over MCP-shaped tool calls with an in-process remote
   broker test.
4. SSH stdio transport and clean-channel MCP initialize/tool-call checks.
5. Background warmup and cached health.
6. Host routing for local versus remote singleton runtimes.
7. Remote workspace/session creation forwarding.
8. Remote turn forwarding and event mirroring.
9. Remote request resolution, cancellation, and cleanup forwarding.

Remaining follow-up:

1. Reusable per-host connection handles instead of one SSH process per operation.
2. Operation-status querying for crash recovery after an ambiguous accepted
   remote mutation.
3. Strong remote broker identity persistence beyond the configured host id.
4. Explicit `status --refresh`/doctor probing.
5. Ignored real SSH smoke tests in environments with a configured localhost SSH
   target.

## AHP connector direction

The first AHP integration should remain a separate `AhpHostConnector` direction:
singleton acts as an AHP client to an external host and normalizes resource
channels into singleton resources/events. Do not put AHP protocol types in
`singleton-core` until the protocol stabilizes.
