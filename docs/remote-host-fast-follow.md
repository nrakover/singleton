# Remote Host Fast Follow

Remote execution is outside the MVP runtime path, but the Rust workspace now
has a concrete host seam for it: `HostConnector` plus an SSH-specific
`RemoteRunner` abstraction in `singleton-host`.

## Goals

- Keep MCP tools stable when host placement changes.
- Treat host placement separately from agent backend choice.
- Support repo-homed sessions through remote workspaces/worktrees.
- Preserve ordered event/reconnect assumptions so AHP can be added later.
- Keep secrets out of SQLite; store only references such as SSH target names,
  provider ids, or keychain references.

## Host runner protocol

The fast-follow runner protocol should have these properties:

1. Stable host id and advertised capabilities.
2. Reliable command dispatch with stdout, stderr, exit status, and structured
   error mapping.
3. Ordered event stream from each remote singleton worker/broker component.
4. Reconnect using last-seen sequence number.
5. Explicit workspace provider capabilities.
6. Auth material referenced externally, never copied into singleton state.

The current Rust scaffold starts with:

```rust
#[async_trait]
pub trait RemoteRunner: Send + Sync {
    async fn run(
        &self,
        target: &str,
        ssh_args: &[String],
        command: &str,
    ) -> Result<RemoteCommandOutput>;
}
```

`SshRemoteRunner` implements that trait with the local `ssh` binary.
`SshHostConnector<R>` accepts any runner, which keeps tests deterministic and
lets future runners use native SSH libraries or cloud APIs.

## SSH connector behavior

`SshHostConnector` currently supports:

- `local_path`: verifies a remote directory with `test -d`
- `git_worktree`: runs remote `git worktree add`
- `backend_default`: creates a workspace record with no path
- `close_resource(..., disposition: "delete")`: runs remote
  `git worktree remove`

The connector only constructs and dispatches remote workspace commands. It does
not yet install `singletond`, start remote agent runtimes, stream remote events,
or tunnel MCP. Those belong to the next remote vertical slice.

## Cloud sandbox candidates

Cloud providers should be wrapped as host connectors, not agent backends.

Evaluation criteria:

- Can provision a repo checkout or worktree-equivalent quickly.
- Can expose a stable workspace id and cleanup policy.
- Provides logs/events or lets singleton run a remote event forwarder.
- Supports reconnect after foreground context loss.
- Has clear auth boundary with token/key references.
- Can cleanly map provider lifecycle to `archive`, `dispose`, and `delete`.

Candidate paths:

- GitHub-hosted/Copilot cloud sessions for Copilot-native remote execution.
- Daytona-like sandbox providers for general repo sandboxes.
- SSH-accessible developer boxes as the simplest first remote host.

## AHP connector direction

The first AHP integration should be `AhpHostConnector`: singleton acts as an
AHP client to an external host and normalizes root/session/chat/terminal and
changeset channels into singleton resources/events.

Do not put AHP protocol types in `singleton-core` yet. Keep AHP-specific code
behind a connector crate/feature until the protocol stabilizes.

Mapping:

| AHP concept | singleton concept |
|---|---|
| root | broker capabilities/catalogues |
| host/server | host connector |
| workspace/files | workspace |
| session | session |
| chat | chat |
| action stream | sequence-numbered events |
| terminal | terminal resource |
| changeset | changeset resource |
| auth/protected resource | host/backend auth reference |

## Next implementation slice

1. Add a remote singleton bootstrap command that can install/start a remote
   event forwarder over SSH.
2. Persist SSH host configs with auth references.
3. Add integration tests using a local fake SSH runner and, optionally, a real
   localhost SSH target behind an ignored test.
4. Add remote event ingestion into `singleton-store.events`.
5. Decide whether remote sessions run a full `singletond` peer or a smaller
   runner process.
6. Prototype `AhpHostConnector` only after the SSH runner proves the
   host/workspace/session boundaries.
