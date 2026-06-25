# singleton - Test Invariant Catalogue

## 1. Scope and catalogue discipline

The current behavioral target is the Rust/Copilot MCP session broker. The
previous Python pytest suite belongs to the legacy prototype and is not an
authoritative verification gate.

This document is a language-agnostic catalogue of hard invariants. Each entry
must stay close enough to executable assertions that a maintainer can map the
entry to tests without reading intent into broad coverage bullets.

Every current and future entry must include:

- **Status**: `Enforced`, `Partially enforced`, `Planned`, or `Historical`.
- **Executable anchors**: test names or `none`.
- **Preconditions**: the required initial state and inputs.
- **Postconditions**: the observable outcomes after the operation.
- **Invariants**: the hard rule executable tests must assert formally.

If no executable test enforces an entry, mark it `Planned` instead of implying
coverage. When adding executable tests, either point them at an existing entry
or update this catalogue in the same change.

The default verification gate is:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets
```

Live Copilot tests are opt-in and must never run in the default CI gate:

```bash
cargo test --workspace --features live-copilot -- --ignored
```

---

## 2. Enforced executable invariants by layer

### 2.0 Config layer

- **G1. Config defaults and precedence are deterministic**
  - **Status**: Planned.
  - **Executable anchors**: none.
  - **Preconditions**: Load no config, user config, project config, environment
    overrides, and CLI/MCP request overrides across supported path locations.
  - **Postconditions**: No config is valid and synthesizes the default profile;
    precedence is built-in defaults < user config < project config < env vars <
    CLI args/MCP request fields; invalid versions, profile refs, host refs,
    repo aliases, enum values, and path combinations fail explicitly.
  - **Invariants**: Runtime defaults, advertised MCP defaults, CLI rendering,
    backend selection, and host/workspace placement are all derived from the
    same `EffectiveConfig` object.

- **G2. Config remains safe to load from project files**
  - **Status**: Planned.
  - **Executable anchors**: none.
  - **Preconditions**: Load nearest-ancestor `.singleton.toml` plus SSH host
    declarations that use `kind`, `target`, optional `connect_command`, and
    optional `ssh_args`.
  - **Postconditions**: Project config cannot silently introduce arbitrary
    non-default SSH `connect_command` values; raw passwords, tokens, and private
    key contents are rejected or remain unrepresentable; redacted effective
    config contains no secret-like values.
  - **Invariants**: Project config is declarative and safe-by-schema; config-
    driven host registration never persists raw secret material to SQLite.

- **G3. Repo workspace provider fallback is source-sensitive**
  - **Status**: Planned.
  - **Executable anchors**: none.
  - **Preconditions**: Resolve shorthand workspace defaults with
    `repo_workspace_provider = "git_worktree"` for git repo sources and
    ordinary non-git directories.
  - **Postconditions**: Git repo sources default to isolated worktrees;
    non-git directories fall back to `local_path`; explicit MCP
    `WorkspaceSpec.kind` always wins.
  - **Invariants**: Config defaults can fill omitted workspace placement fields
    without overriding explicit tool input or treating arbitrary directories as
    repositories.

### 2.1 Core model layer (`singleton-core`)

- **C1. Stable resource URI syntax**
  - **Status**: Enforced.
  - **Executable anchors**: `resource_uri_uses_stable_scheme`.
  - **Preconditions**: Build a root URI with any id and a session URI with
    `sess_123`.
  - **Postconditions**: Root is `singleton-root://`; session is
    `singleton-session:/sess_123`.
  - **Invariants**: Resource URI construction is deterministic; root identity
    is singleton-wide; session identity is encoded in the session URI path.

- **C2. Inbox counts derive from items**
  - **Status**: Enforced.
  - **Executable anchors**: `inbox_counts_are_derived_from_items`.
  - **Preconditions**: Start with an empty inbox and push one unread completed
    turn item.
  - **Postconditions**: `completed_turn` count is `1`; the inbox contains one
    item.
  - **Invariants**: Inbox aggregate counts cannot diverge from item insertion
    semantics.

- **C3. Default MCP tool profile remains compact**
  - **Status**: Enforced.
  - **Executable anchors**: `default_tool_profile_stays_small`.
  - **Preconditions**: Inspect the default tool list.
  - **Postconditions**: The list has exactly 13 tools and includes
    `get_inbox`, `ack_inbox`, `get_latest_output`, and `close_resource`.
  - **Invariants**: The default MCP surface remains intentionally small; adding
    or removing default tools is a spec-visible compatibility change.

- **C4. Planned core model invariants**
  - **Status**: Planned.
  - **Executable anchors**: none.
  - **Preconditions**: Exercise id creation, lifecycle transitions, close rules,
    request decisions, event cursor ordering, validation errors, and capability
    serialization.
  - **Postconditions**: Invalid operations fail explicitly; valid operations
    produce stable serializable state.
  - **Invariants**: Resource IDs and URIs are unique; host/workspace/session/
    chat/turn/request identities remain separate; invalid transitions are not
    accepted; close is idempotent; destructive workspace deletion requires an
    explicit disposition and no active references unless forced.

### 2.2 Store layer (`singleton-store`)

- **S1. Migrations initialize an empty database**
  - **Status**: Enforced.
  - **Executable anchors**: `migrations_apply_to_empty_database`.
  - **Preconditions**: Open a temporary empty SQLite database.
  - **Postconditions**: Appending the first event succeeds and assigns
    `server_seq = 1`.
  - **Invariants**: The schema is complete after opening an empty database;
    persisted event sequence numbers start at one.

- **S2. Workspace/session/turn round-trip**
  - **Status**: Enforced.
  - **Executable anchors**: `workspace_session_turn_roundtrip`.
  - **Preconditions**: Insert a ready workspace, a session attached to that
    workspace, and a new turn for the session.
  - **Postconditions**: Workspace path and session title read back unchanged;
    no active turn is reported for the non-running turn.
  - **Invariants**: Core records are durable and readable; active-turn queries
    are status-sensitive rather than "latest turn" aliases.

- **S3. Event reads include child resources by parent**
  - **Status**: Enforced.
  - **Executable anchors**: `event_cursor_filters_by_parent_resource`.
  - **Preconditions**: Append one turn event whose parent is a session URI and
    one unrelated session event.
  - **Postconditions**: Reading events for the session URI returns only the turn
    event.
  - **Invariants**: Resource-scoped event reads include direct resource events
    and child events with matching parent resource, while excluding unrelated
    resources.

- **S4. Request resolution removes pending work**
  - **Status**: Enforced.
  - **Executable anchors**: `request_resolution_is_idempotently_readable`.
  - **Preconditions**: Insert one pending permission request.
  - **Postconditions**: Resolving with a deny decision stores
    `status = Resolved`; pending request count becomes zero.
  - **Invariants**: Resolved requests are no longer actionable pending work and
    remain readable through stored state.

- **S5. Planned store invariants**
  - **Status**: Planned.
  - **Executable anchors**: none.
  - **Preconditions**: Persist singleton intents, event filters, request
    resolution races, archived/deleted resources, and daemon restart state.
  - **Postconditions**: Intents are durable before backend dispatch; event reads
    honor cursor, resource, limit, type filter, and wait timeout; archived or
    deleted resources remain queryable for audit and idempotency; restart can
    reconstruct active state from SQLite.
  - **Invariants**: Store operations that define orchestration state are atomic,
    monotonic, and restart-safe.

### 2.3 Host/workspace layer (`singleton-host`)

- **H1. Local path workspaces require existing directories**
  - **Status**: Enforced.
  - **Executable anchors**: `local_path_requires_existing_directory`.
  - **Preconditions**: Provide an existing temporary directory as a local path
    workspace spec.
  - **Postconditions**: Workspace resolution succeeds and records that exact
    path.
  - **Invariants**: Local workspace resolution must preserve the filesystem path
    it accepts.

- **H2. Local git worktree creation and deletion are idempotent**
  - **Status**: Enforced.
  - **Executable anchors**: `git_worktree_create_and_delete_is_idempotent`.
  - **Preconditions**: Initialize a temporary git repo with an initial commit and
    request a new worktree branch at a target path.
  - **Postconditions**: The worktree contains the committed file; first forced
    delete reports one deleted path; repeated delete reports zero deleted paths.
  - **Invariants**: Worktree creation uses the requested repo/ref/branch/path;
    cleanup can be retried safely.

- **H3. SSH worktree operations use deterministic remote commands**
  - **Status**: Enforced.
  - **Executable anchors**: `ssh_connector_builds_remote_worktree_commands`.
  - **Preconditions**: Use an SSH connector with a recording runner and a git
    worktree spec for repo `/srv/repo`, branch `feature-x`, base `main`, and
    path `/srv/worktrees/feature-x`.
  - **Postconditions**: Exactly two commands are recorded: quoted worktree add
    and forced worktree remove.
  - **Invariants**: Remote workspace lifecycle commands are deterministic and
    quote all paths/refs before execution.

- **H4. SSH control invocation is argv-safe and defaults to stdio**
  - **Status**: Enforced.
  - **Executable anchors**:
    `ssh_control_invocation_uses_default_connect_command`,
    `ssh_control_invocation_preserves_optional_ssh_args`,
    `ssh_control_invocation_keeps_connect_command_as_single_argument`.
  - **Preconditions**: Build SSH host configs with default connect command,
    optional `ssh_args`, and a trusted non-default `connect_command`.
  - **Postconditions**: The connector builds `ssh [ssh_args...] target
    connect_command`; the default command is `singleton serve --stdio`; optional
    SSH args and the connect command remain distinct argv items.
  - **Invariants**: No local shell parses the SSH target, SSH args, or connect
    command; non-default connect commands are explicit trusted-user input.

- **H5. SSH config rejects unsafe project or argument input**
  - **Status**: Enforced.
  - **Executable anchors**:
    `ssh_control_invocation_rejects_unsafe_target`,
    `ssh_control_invocation_rejects_newlines_in_ssh_args`,
    `project_config_cannot_override_connect_command`,
    `project_config_cannot_set_ssh_args`,
    `project_config_can_use_default_connect_command`.
  - **Preconditions**: Build SSH host configs with an option-looking target,
    newline-bearing `ssh_args`, a project-sourced non-default connect command,
    project-sourced free-form `ssh_args`, and a project-sourced default connect
    command.
  - **Postconditions**: Unsafe target and newline-bearing args fail explicitly;
    project config cannot override `connect_command` or set `ssh_args`; project
    config can use the default `singleton serve --stdio`.
  - **Invariants**: Project config cannot silently introduce arbitrary remote
    commands or local ssh options, and SSH argv fields reject control-character
    injection.

- **H6. SSH stdio transport is fakeable without an SSH server**
  - **Status**: Enforced.
  - **Executable anchors**:
    `ssh_control_surface_uses_injected_stdio_transport_for_fake_mcp`.
  - **Preconditions**: Use an SSH connector with an injected scripted process
    transport that returns one JSON-RPC line.
  - **Postconditions**: The connector records the expected SSH invocation, writes
    the request to fake stdin, reads the scripted response from fake stdout, and
    observes a successful fake process exit.
  - **Invariants**: Remote MCP/session behavior at the SSH process boundary is
    testable without real SSH credentials, networking, or a remote daemon.

- **H7. SSH worktrees require explicit remote paths until allocation exists**
  - **Status**: Enforced.
  - **Executable anchors**:
    `ssh_git_worktree_requires_explicit_worktree_path_hint`.
  - **Preconditions**: Request an SSH git worktree without `worktree_path_hint`.
  - **Postconditions**: The request fails explicitly and no remote command is
    dispatched.
  - **Invariants**: Local SSH config does not smuggle remote state directories or
    invent remote worktree roots.

- **H8. Shell quoting preserves single quotes**
  - **Status**: Enforced.
  - **Executable anchors**: `shell_quote_handles_single_quotes`.
  - **Preconditions**: Quote the string `a'b`.
  - **Postconditions**: The result is `'a'"'"'b'`.
  - **Invariants**: Shell quoting must produce a single shell token that
    preserves embedded single quotes.

- **H9. Planned host/workspace invariants**
  - **Status**: Planned.
  - **Executable anchors**: none for the exact cases below.
  - **Preconditions**: Exercise branch/base-ref metadata, `keep`,
    `delete_on_archive`, and `delete_on_success` cleanup policies, forced
    cleanup, active session references, invalid repos, invalid paths, and invalid
    base refs.
  - **Postconditions**: Metadata is recorded; policy-specific cleanup occurs
    only when allowed; invalid specs fail explicitly.
  - **Invariants**: Workspace lifecycle is safe by default; destructive cleanup
    requires explicit permission or force; invalid host inputs do not produce
    partial workspace state.

### 2.4 Fake backend layer (`singleton-test-support`)

- **F1. Fake backend emits deterministic completion**
  - **Status**: Enforced.
  - **Executable anchors**: `fake_backend_emits_deterministic_completion`.
  - **Preconditions**: Create a fake backend session and send one message with a
    known turn id.
  - **Postconditions**: The returned turn is completed and contains exactly one
    event.
  - **Invariants**: The fake backend is deterministic enough for broker tests to
    assert terminal state without real Copilot credentials, network, or model
    behavior.

- **F2. Planned fake backend invariants**
  - **Status**: Partially enforced through broker tests; planned as direct fake
    backend tests.
  - **Executable anchors**:
    `latest_output_returns_fake_completion_summary`,
    `latest_output_returns_fake_failure_summary`,
    `latest_output_marks_completed_turn_without_text_for_event_inspection`,
    `permission_request_flows_to_inbox_and_resolves`,
    `cancel_turn_cancels_pending_requests`,
    `broker_startup_reattaches_active_turn_when_backend_supports_it`.
  - **Preconditions**: Drive fake session resume, streaming deltas, success,
    no-output completion, failure, cancellation, permission request, input
    request, and missing backend state.
  - **Postconditions**: Fake behavior is repeatable and emits the expected
    normalized events for broker assertions.
  - **Invariants**: Fake backend scenarios must be deterministic and complete
    enough that default tests never depend on live Copilot.

### 2.5 Broker orchestration layer (`singleton-broker`)

- **B1. Create/send/read-events fake vertical slice**
  - **Status**: Enforced.
  - **Executable anchors**: `create_send_and_read_events_with_fake_backend`.
  - **Preconditions**: Create a broker with in-memory store, fake backend, local
    host, and a temporary local-path workspace; create a session and send a
    message.
  - **Postconditions**: Send reply status is running; reading from the returned
    cursor with `turn.completed` filter returns exactly one completion event;
    inbox completed-turn count is one.
  - **Invariants**: Broker sends are asynchronous at the API boundary, event
    reads honor cursor and type filter, and terminal turns create unread inbox
    work.

- **B2. Compact latest output for successful turns**
  - **Status**: Enforced.
  - **Executable anchors**: `latest_output_returns_fake_completion_summary`.
  - **Preconditions**: Fake backend completes a turn with summary
    `finished compactly`.
  - **Postconditions**: Latest output references that turn, status is completed,
    result text is the summary, source is `turn_summary`,
    `needs_event_inspection` is false, and source event is `turn.completed`.
  - **Invariants**: Completed turn summaries are returned as compact latest
    output without requiring transcript/event inspection.

- **B3. Compact latest output for failed turns**
  - **Status**: Enforced.
  - **Executable anchors**: `latest_output_returns_fake_failure_summary`.
  - **Preconditions**: Fake backend fails a turn with summary
    `backend failed deterministically`.
  - **Postconditions**: Requested turn output has failed status, failure text,
    `turn_summary` source, and `needs_event_inspection = false`.
  - **Invariants**: Failed turns expose compact failure summaries through the
    same latest-output contract as successful turns.

- **B4. No-output completion requires event inspection**
  - **Status**: Enforced.
  - **Executable anchors**:
    `latest_output_marks_completed_turn_without_text_for_event_inspection`.
  - **Preconditions**: Fake backend completes a turn without an output payload.
  - **Postconditions**: Status is completed; result text is absent; source is
    `none`; `needs_event_inspection` is true; cursor is at least the send cursor;
    source event is `turn.completed`.
  - **Invariants**: Singleton must not invent success text; absence of compact
    output is explicit and points callers to event inspection.

- **B5. Empty sessions have typed no-turn latest output**
  - **Status**: Enforced.
  - **Executable anchors**: `latest_output_returns_no_turn_metadata_for_empty_session`.
  - **Preconditions**: Create a session and do not send any turns.
  - **Postconditions**: Latest output returns the session id, no turn id, no
    status, no result text, `none` source, `needs_event_inspection = false`, and
    the session creation cursor.
  - **Invariants**: Asking for latest output on an empty session is a typed empty
    result, not an error.

- **B6. Permission requests flow through inbox and resolve**
  - **Status**: Enforced.
  - **Executable anchors**: `permission_request_flows_to_inbox_and_resolves`.
  - **Preconditions**: Fake backend emits a permission request during a turn.
  - **Postconditions**: `get_inbox` reports one permission request; resolving it
    with approve stores `RequestStatus::Resolved`.
  - **Invariants**: Permission requests are actionable inbox items and explicit
    resolution transitions them out of pending state.

- **B7. Workspace delete refuses active sessions**
  - **Status**: Enforced.
  - **Executable anchors**: `workspace_delete_refuses_active_session`.
  - **Preconditions**: Create a session bound to a workspace and leave the
    session active.
  - **Postconditions**: Deleting the workspace without force returns an error.
  - **Invariants**: Destructive workspace cleanup cannot proceed while an active
    session references the workspace unless a force path is tested and allowed.

- **B8. AHP-like session snapshots use resource links**
  - **Status**: Enforced.
  - **Executable anchors**: `ahp_like_snapshot_uses_resource_links`.
  - **Preconditions**: Create a broker session and convert its detail to an
    AHP-like snapshot.
  - **Postconditions**: Snapshot kind is `session`; snapshot resource equals the
    session resource URI.
  - **Invariants**: Internal state remains projectable into resource-linked
    snapshots without importing an AHP runtime dependency.

- **B9. Broker startup marks stale active turns interrupted**
  - **Status**: Enforced.
  - **Executable anchors**: `broker_startup_marks_stale_active_turns_interrupted`.
  - **Preconditions**: Store a running session and running turn, then construct a
    broker without active-turn reattach.
  - **Postconditions**: Turn is failed and unread; session becomes idle; one
    `turn.failed` event is appended for the turn.
  - **Invariants**: Restart must not leave orphaned running turns; unrecoverable
    active turns become visible retryable failures.

- **B10. Acknowledging inbox marks completed turns read**
  - **Status**: Enforced.
  - **Executable anchors**: `ack_inbox_marks_completed_turns_read`.
  - **Preconditions**: Complete a fake turn and observe one completed-turn inbox
    item.
  - **Postconditions**: Acknowledging that turn reports one acknowledged item and
    completed-turn inbox count becomes zero.
  - **Invariants**: Inbox acknowledgement changes unread terminal turns into
    read terminal turns without deleting turn history.

- **B11. Cancelling turns cancels pending requests**
  - **Status**: Enforced.
  - **Executable anchors**: `cancel_turn_cancels_pending_requests`.
  - **Preconditions**: Fake backend creates a permission request for a running
    turn.
  - **Postconditions**: Cancelling the turn removes the permission request from
    inbox and appends a `request.cancelled` event.
  - **Invariants**: Turn cancellation must clean up actionable requests tied to
    that turn.

- **B12. Broker startup reattaches active turns when backend supports it**
  - **Status**: Enforced.
  - **Executable anchors**:
    `broker_startup_reattaches_active_turn_when_backend_supports_it`.
  - **Preconditions**: Store a running session and running turn with backend ids,
    then construct a broker through reconnect-capable startup.
  - **Postconditions**: Turn becomes completed and unread; session becomes idle;
    exactly two relevant events are appended: `turn.reattached` and
    `turn.completed`.
  - **Invariants**: When backend reattach is available, restart should recover
    active work instead of marking it interrupted.

- **B13. Planned broker invariants**
  - **Status**: Planned.
  - **Executable anchors**: none for the exact cases below.
  - **Preconditions**: Exercise list/get session summaries, close-session
    idempotency, close-resource forced cleanup, all request decisions
    (`approve`, `deny`, `respond`, `cancel`), backend session disappearance, and
    multi-session fan-in.
  - **Postconditions**: Replies are typed and compact; failed/degraded state is
    explicit; idempotent operations stay idempotent; no transcript is invented
    from normalized events.
  - **Invariants**: Singleton owns orchestration state and workspace lifecycle,
    while backend transcript persistence remains backend-owned.

### 2.6 MCP facade layer (`singleton-mcp`)

- **M1. MCP tool list includes the default profile**
  - **Status**: Enforced.
  - **Executable anchors**: `default_tool_profile_matches_spec`.
  - **Preconditions**: Create an in-process MCP server over an in-memory broker.
  - **Postconditions**: Server tool names contain every name in the default tool
    profile.
  - **Invariants**: The MCP facade exposes the compact default tool surface
    defined by the core model.

- **M2. Typed MCP facade vertical slice**
  - **Status**: Enforced.
  - **Executable anchors**: `typed_mcp_facade_runs_vertical_slice`.
  - **Preconditions**: Create an in-process MCP server, create a local-path
    session, and send a message.
  - **Postconditions**: Send reply is running; reading events returns a
    `turn.completed` event; latest output contains `fake turn completed` and
    does not require event inspection.
  - **Invariants**: The typed MCP facade delegates through the same broker paths
    as direct broker calls.

- **M3. Planned MCP contract invariants**
  - **Status**: Planned.
  - **Executable anchors**: none for the exact cases below.
  - **Preconditions**: Validate tool schemas, invalid inputs, full
    `get_capabilities`, compact `get_inbox`, inline workspace equivalence,
    `read_events(wait_ms=...)`, every `resolve_request` decision,
    `ack_inbox` idempotency, and `close_resource` idempotency.
  - **Postconditions**: Inputs and outputs are stable and machine-readable;
    invalid inputs return typed errors; wait semantics do not require a separate
    wait tool.
  - **Invariants**: MCP is a compact protocol boundary, not a transcript API or
    an alternate implementation path.

### 2.7 Copilot adapter layer (`singleton-copilot`)

- **P1. Copilot adapter reports real backend capabilities**
  - **Status**: Enforced.
  - **Executable anchors**: `adapter_reports_real_copilot_capabilities`.
  - **Preconditions**: Instantiate the Copilot backend adapter.
  - **Postconditions**: Backend id is Copilot; resume, cancel, and permissions
    are supported; active-turn reattach is not supported.
  - **Invariants**: Broker restart and permission behavior can branch on an
    explicit adapter capability contract.

- **P2. Request wait timeout cancels pending input**
  - **Status**: Enforced.
  - **Executable anchors**: `request_wait_timeout_cancels_pending_request`.
  - **Preconditions**: Insert a pending input request and use a store request
    broker with zero timeout.
  - **Postconditions**: Waiting returns a cancelled request and the stored
    request status is cancelled.
  - **Invariants**: Adapter-side request waits do not leave stale actionable
    requests after timeout.

- **P3. Live Copilot create/send smoke**
  - **Status**: Enforced only when `live-copilot` is enabled and ignored tests
    are explicitly requested.
  - **Executable anchors**: `live_copilot_create_and_send_session`.
  - **Preconditions**: Authenticated Copilot CLI access is available.
  - **Postconditions**: A live session is created; sending a bounded-time smoke
    turn returns completed or cancelled status.
  - **Invariants**: Live Copilot checks are opt-in smoke tests and cannot affect
    the default deterministic gate.

- **P4. Planned Copilot adapter invariants**
  - **Status**: Planned.
  - **Executable anchors**: none.
  - **Preconditions**: Exercise configuration validation, provider error
    mapping, recorded/fake SDK event normalization, permission handler plumbing,
    resume, cancellation, and permission resolution.
  - **Postconditions**: Provider failures are mapped to typed singleton errors;
    SDK events normalize into stable broker events; live tests remain feature
    gated.
  - **Invariants**: Copilot SDK state is authoritative for transcripts, while
    singleton persists only orchestration state and normalized events.

### 2.8 CLI layer (`singleton-cli`)

- **L1. Explicit database path is honored**
  - **Status**: Enforced.
  - **Executable anchors**: `explicit_database_path_is_used`.
  - **Preconditions**: Resolve a user-provided database path.
  - **Postconditions**: The resolved database path equals the provided path.
  - **Invariants**: Explicit state locations override defaults.

- **L2. State files derive from explicit database path**
  - **Status**: Enforced.
  - **Executable anchors**: `explicit_database_derives_pid_and_socket_paths`.
  - **Preconditions**: Resolve daemon state paths from an explicit database path.
  - **Postconditions**: Database path is unchanged; pid path has `.pid`
    extension; socket path has `.sock` extension.
  - **Invariants**: Daemon lifecycle files colocate predictably with explicit
    database state.

- **L3. MCP install command for Copilot**
  - **Status**: Enforced.
  - **Executable anchors**: `install_mcp_builds_copilot_command`.
  - **Preconditions**: Build a dry-run install command for Copilot, Copilot
    backend, and `/usr/local/bin/singleton`.
  - **Postconditions**: Program is `copilot`; args are `mcp add singleton --`
    followed by `singleton serve --stdio --backend copilot`.
  - **Invariants**: Copilot registration invokes singleton through stdio with
    the selected backend.

- **L4. MCP install command for Claude with database**
  - **Status**: Enforced.
  - **Executable anchors**: `install_mcp_builds_claude_command_with_database`.
  - **Preconditions**: Build a dry-run install command for Claude, fake backend,
    explicit database, and `/opt/singleton/bin/singleton`.
  - **Postconditions**: Program is `claude`; args include `--transport stdio`,
    service name, singleton stdio serve command, fake backend, and database
    path.
  - **Invariants**: Non-Copilot client registration preserves backend and
    database configuration explicitly.

- **L5. MCP install command for Codex**
  - **Status**: Enforced.
  - **Executable anchors**: `install_mcp_builds_codex_command`.
  - **Preconditions**: Build a dry-run install command for Codex and Copilot
    backend.
  - **Postconditions**: Program is `codex`; args are `mcp add singleton --`
    followed by `singleton serve --stdio --backend copilot`.
  - **Invariants**: Codex registration uses the same stdio server contract as
    other MCP clients.

- **L6. Dry-run command rendering is shell-safe**
  - **Status**: Enforced.
  - **Executable anchors**: `command_spec_renders_shell_safe_dry_run`.
  - **Preconditions**: Render a command with spaces in the MCP service name and
    binary path.
  - **Postconditions**: Rendered text quotes the arguments that contain spaces.
  - **Invariants**: Human-readable dry-run output must remain copy/paste safe
    for shell execution.

- **L7. Fake daemon lifecycle is idempotent and isolated**
  - **Status**: Enforced.
  - **Executable anchors**: `cli_start_status_stop_fake_daemon`.
  - **Preconditions**: Start the fake daemon against a temporary database, repeat
    start, check status, then stop twice.
  - **Postconditions**: Status reports running with pid and listening socket;
    daemon pid owns its process group; repeated stop succeeds; final status is
    stopped.
  - **Invariants**: Daemon start/stop operations are idempotent and auto-started
    daemons are isolated in their own process group.

- **L8. Stale lifecycle files are reported and cleaned**
  - **Status**: Enforced.
  - **Executable anchors**:
    `cli_status_reports_and_stop_cleans_stale_lifecycle_files`.
  - **Preconditions**: Create stale pid and socket files for a database path.
  - **Postconditions**: Status reports stale pid/socket and cleanup guidance;
    `stop` removes each stale file; final status is stopped.
  - **Invariants**: Stale daemon state is visible to users and cleanup is safe.

- **L9. Concurrent daemon starts serialize to one running daemon**
  - **Status**: Enforced.
  - **Executable anchors**: `cli_concurrent_fake_daemon_starts_are_idempotent`.
  - **Preconditions**: Launch multiple concurrent fake daemon starts for the
    same database.
  - **Postconditions**: All starts succeed and status reports one running
    daemon.
  - **Invariants**: Daemon startup is concurrency-safe and idempotent.

- **L10. Stdio MCP fake backend vertical slice**
  - **Status**: Enforced.
  - **Executable anchors**: `stdio_mcp_serves_fake_backend_vertical_slice`.
  - **Preconditions**: Spawn `singleton serve --backend fake --stdio` with a
    temporary database and initialize JSON-RPC MCP.
  - **Postconditions**: `tools/list` includes `create_session`, `send_message`,
    `read_events`, and `get_latest_output`; `get_capabilities` reports protocol
    version `0.1`; create returns a session id; send status is running;
    `read_events` observes `turn.completed`; latest output is `fake turn
    completed` with `turn_summary` source and no event inspection needed.
  - **Invariants**: The binary exposes a working JSON-RPC MCP vertical slice
    over stdio using the fake backend.

- **L11. Live stdio MCP Copilot smoke**
  - **Status**: Enforced only when `live-copilot` is enabled and ignored tests
    are explicitly requested.
  - **Executable anchors**: `live_stdio_mcp_serves_copilot_backend`.
  - **Preconditions**: Spawn `singleton serve --backend copilot --stdio` with
    authenticated Copilot CLI access.
  - **Postconditions**: A live session can be created; sending a message returns
    running status; a `turn.completed` event is observed.
  - **Invariants**: Live MCP/Copilot coverage is an opt-in smoke test outside
    the default deterministic gate.

- **L12. Planned CLI invariants**
  - **Status**: Planned.
  - **Executable anchors**: none for the exact cases below.
  - **Preconditions**: Exercise `singleton serve --backend copilot` backend
    selection without live credentials, stdio proxy-to-daemon disconnect
    behavior, `singleton status` state summaries beyond daemon status,
    `singleton mcp-config --backend copilot`, and broader human-readable output
    snapshots.
  - **Postconditions**: CLI output remains stable for smoke tests; proxy
    disconnect does not stop broker-owned turns; generated MCP config is
    structurally correct.
  - **Invariants**: CLI commands are thin, stable surfaces over broker state and
    MCP registration.

---

## 3. End-to-end scenario catalogue

These scenarios should run with fake backend and temporary local workspaces
unless explicitly marked live.

### 3.1 Fresh worktree session

- **Status**: Partially enforced.
- **Executable anchors**: `git_worktree_create_and_delete_is_idempotent`,
  `create_send_and_read_events_with_fake_backend`,
  `typed_mcp_facade_runs_vertical_slice`,
  `stdio_mcp_serves_fake_backend_vertical_slice`,
  `ack_inbox_marks_completed_turns_read`.
- **Preconditions**: Create a temporary git repo, create a worktree-backed
  session, send one message, read events to completion, inspect latest output,
  acknowledge inbox, close the session, and delete the workspace.
- **Postconditions**: Workspace/session records exist; send returns running;
  completion is observable through events; latest output is compact or explicitly
  requires event inspection; completion inbox item can be acknowledged; workspace
  deletion is explicit and idempotent.
- **Invariants**: The first-run workflow must be deterministic and cleanup-safe.
  The exact single test that covers the full close-and-delete sequence is
  planned; current tests enforce the component invariants.

### 3.2 Parallel fan-in

- **Status**: Partially enforced.
- **Executable anchors**: `create_send_and_read_events_with_fake_backend`,
  `latest_output_returns_fake_completion_summary`,
  `latest_output_returns_fake_failure_summary`,
  `permission_request_flows_to_inbox_and_resolves`,
  `cancel_turn_cancels_pending_requests`.
- **Preconditions**: Create three sessions, send three turns, complete one, fail
  one, and leave one awaiting input or permission.
- **Postconditions**: Inbox represents completed, failed, and request work;
  latest output works for completed and failed turns; resolving or cancelling the
  request appends a terminal request event.
- **Invariants**: Fan-in summaries must be compact and actionable without
  transcript reads. A single multi-session test is planned; current tests enforce
  single-session components.

### 3.2.1 Compact latest output

- **Status**: Enforced.
- **Executable anchors**: `latest_output_returns_fake_completion_summary`,
  `latest_output_returns_fake_failure_summary`,
  `latest_output_marks_completed_turn_without_text_for_event_inspection`,
  `latest_output_returns_no_turn_metadata_for_empty_session`,
  `typed_mcp_facade_runs_vertical_slice`,
  `stdio_mcp_serves_fake_backend_vertical_slice`.
- **Preconditions**: Query latest output for completed, failed, no-output, and
  no-turn sessions through broker/MCP surfaces.
- **Postconditions**: Completed and failed turns return compact summaries;
  no-output completion returns `needs_event_inspection = true`; no-turn session
  returns a typed empty result.
- **Invariants**: Latest output is compact, typed, and honest about whether
  event inspection is required.

### 3.3 Resume after restart

- **Status**: Partially enforced.
- **Executable anchors**: `broker_startup_marks_stale_active_turns_interrupted`,
  `broker_startup_reattaches_active_turn_when_backend_supports_it`.
- **Preconditions**: Persist running sessions and turns, restart broker state,
  and test both backend reattach support and no-reattach support.
- **Postconditions**: Reattach-capable backends append `turn.reattached` then
  `turn.completed`; non-reattach startup marks active turns failed/unread and
  sessions idle.
- **Invariants**: Restart cannot leave invisible active work. Tests for a full
  drop-and-reopen SQLite daemon restart and pending-request cancellation during
  interrupted recovery are planned.

### 3.4 Backend state missing

- **Status**: Planned.
- **Executable anchors**: none.
- **Preconditions**: Persist a session with a backend id mapping, restart the
  broker, and have the backend report that the mapped backend session is
  missing.
- **Postconditions**: Singleton marks the session degraded or broken and does
  not reconstruct backend transcript state from normalized singleton events.
- **Invariants**: Copilot/backend transcript persistence remains backend-owned;
  singleton does not synthesize missing backend history.

### 3.5 Workspace cleanup safety

- **Status**: Partially enforced.
- **Executable anchors**: `workspace_delete_refuses_active_session`,
  `git_worktree_create_and_delete_is_idempotent`.
- **Preconditions**: Share one workspace across sessions, close sessions in
  sequence, attempt deletion before and after all references are inactive, and
  retry deletion.
- **Postconditions**: Delete without force fails while any active session
  remains; delete succeeds once safe; repeated delete is successful and reports
  no new paths.
- **Invariants**: Workspace deletion is conservative, explicit, and idempotent.
  The exact shared-workspace two-session scenario is planned.

### 3.6 Copilot plugin smoke

- **Status**: Planned.
- **Executable anchors**: none.
- **Preconditions**: Build or install a local singleton binary, add a clean repo
  as a Copilot plugin marketplace, install `singleton@singleton`, start Copilot
  CLI, inspect MCP tools and Skill inventory, call a minimal MCP flow, and
  uninstall.
- **Postconditions**: Plugin installation succeeds; singleton MCP tools and
  packaged Skill are visible; `get_capabilities`, `create_session`,
  `send_message`, and `read_events` work through the plugin packaging.
- **Invariants**: Distribution packaging must expose the same broker/MCP
  behavior as local development binaries.

---

## 4. Packaging and plugin catalogue

- **G1. Release and plugin packaging**
  - **Status**: Planned.
  - **Executable anchors**: none under `crates/**/src` or `crates/**/tests`.
  - **Preconditions**: Run release workflow and plugin packaging checks for
    supported macOS/Linux targets, release archives, marketplace manifest,
    plugin manifest, Skill frontmatter, MCP config, launcher script, and local
    Copilot plugin install.
  - **Postconditions**: Release publishes `.tar.gz` archives and `.sha256`
    files for `v*.*.*` tags; archives contain an executable `singleton`; plugin
    manifests point to the plugin subdirectory, MCP config, and skills
    directory; `skills/singleton/SKILL.md` has valid frontmatter; launcher starts
    through `bash`, passes shell syntax checks, writes bootstrap diagnostics to
    stderr, and honors `SINGLETON_BINARY`, `SINGLETON_VERSION`,
    `SINGLETON_RELEASE_BASE_URL`, `SINGLETON_FORCE_INSTALL`,
    `SINGLETON_BACKEND`, and `SINGLETON_DATABASE`.
  - **Invariants**: Packaging tests must not download release assets in the
    default unit gate. Networked release/download checks belong in release or
    manual smoke validation.

---

## 5. AHP alignment catalogue

AHP is not an MVP runtime dependency. Tests in this section must not import AHP
crates unless a future optional adapter is added behind a feature flag.

- **A1. Session snapshot resource projection**
  - **Status**: Enforced.
  - **Executable anchors**: `ahp_like_snapshot_uses_resource_links`.
  - **Preconditions**: Convert a session detail into an AHP-like snapshot.
  - **Postconditions**: Snapshot kind is `session`; resource equals the stable
    singleton session URI.
  - **Invariants**: Singleton state remains projectable through stable resource
    links.

- **A2. Planned AHP projection invariants**
  - **Status**: Planned.
  - **Executable anchors**: none.
  - **Preconditions**: Project root, session, changeset, event sequence, and
    reconnect cursor state into AHP-like snapshots/action streams without AHP
    runtime imports.
  - **Postconditions**: Root snapshot includes hosts, capabilities, and resource
    links; session snapshot includes chats, turns, status, and workspace
    reference; changeset snapshot includes metadata and resource URI; event
    sequences replay from a cursor; reconnect requests events after the last
    seen sequence.
  - **Invariants**: Future AHP adapters must be projections of singleton's
    stable resource/event model, not a separate orchestration state store.

---

## 6. Legacy Python tests

- **Y1. Legacy tests are historical only**
  - **Status**: Historical.
  - **Executable anchors**: none for the Rust/Copilot broker gate.
  - **Preconditions**: Legacy Python/Claude daemon or hub tests exist in history
    or archival branches.
  - **Postconditions**: They may inform archaeology but do not define current
    broker behavior and are not blockers for doc-only Rust/Copilot design work.
  - **Invariants**: Do not add new behavior to the old Python daemon/hub
    contracts. Remove or archive legacy tests only in a dedicated cleanup change
    after equivalent Rust coverage exists.
