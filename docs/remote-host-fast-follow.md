# Remote Host Fast Follow

Remote execution is no longer modeled as the local broker running ad hoc shell
commands on another machine. SSH host support should mean local singleton
brokering to a remote singleton control surface over SSH stdio.

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

## Connection lifecycle

On daemon startup/restart, singleton should start bounded, concurrency-limited
warmup attempts for every configured SSH host after the local daemon is ready to
serve MCP. Warmup performs MCP initialize, singleton protocol/capability
negotiation, remote broker identity checks, and remote backend usability checks.
It must not create workspaces or sessions.

Host health must distinguish:

- `not_checked`: host was configured but no attempt has been scheduled yet
- `checking`: startup warmup or refresh is in progress
- `available`: compatible remote singleton and usable backend were seen
- `unavailable`: SSH/MCP failed after a real attempt
- `incompatible`: remote singleton lacks required protocol/capabilities
- `degraded`: mapped resources exist but reconciliation is incomplete
- `backoff`: automatic reconnect is delayed after repeated failures

`get_capabilities` should report cached health and never mark a fresh host
unavailable merely because no connection has been attempted. Host-targeted reads
should attempt bounded reconciliation before returning local mirror data. Broad
fan-in reads should not block on every remote host, but should enqueue refreshes
for dropped/stale hosts with active sessions.

## Reconnect and reconciliation

Reconnect is driven by startup warmup, active remote work, pending forwarded
operations, host-targeted reads/writes, explicit refresh/doctor commands, and
fan-in noticing stale active remote sessions.

A reconnect attempt must:

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

1. Keep SSH config descriptors parsed but truthfully unavailable until runtime
   support exists.
2. Add federation/idempotency metadata and local-to-remote resource links.
3. Add a remote broker client over MCP with fake remote tests.
4. Add SSH stdio transport and clean-channel/compatibility handshake checks.
5. Add a connection manager with startup warmup, cached health, and reconnect
   reconciliation.
6. Add host routing for local versus remote singleton runtimes.
7. Forward workspace/session creation to the remote singleton.
8. Forward turns and mirror remote events into local storage.
9. Forward request resolution, cancellation, and cleanup with remote
   acknowledgement semantics.
10. Add ignored real SSH smoke tests and user-facing docs.

## AHP connector direction

The first AHP integration should remain a separate `AhpHostConnector` direction:
singleton acts as an AHP client to an external host and normalizes resource
channels into singleton resources/events. Do not put AHP protocol types in
`singleton-core` until the protocol stabilizes.
