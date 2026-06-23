# singleton - Test Strategy

## 1. Scope

The current behavioral target is the Rust MCP session broker. The previous
Python pytest suite is historical reference while the rewrite is underway.

The new verification gate is:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets
```

Live Copilot tests must be opt-in:

```bash
cargo test --workspace --features live-copilot -- --ignored
```

---

## 2. Test Layers

### 2.1 Core unit tests

Crate: `singleton-core`

Coverage:

- id and resource URI creation
- host/workspace/session/chat/turn/request state transitions
- close/disposition rules
- request resolution semantics
- event cursor ordering
- error mapping and validation
- capability model serialization

Required properties:

- no duplicate resource URIs
- invalid transitions fail explicitly
- idempotent close operations remain idempotent
- destructive workspace deletion requires explicit disposition and no active
  references unless forced

### 2.2 Store tests

Crate: `singleton-store`

Coverage:

- migrations apply from an empty database
- hosts/workspaces/sessions/chats/turns/requests can be created and read back
- singleton intents are persisted before backend dispatch
- events get monotonic `server_seq` values
- `read_events` respects cursor, target resource, limit, type filter, and wait
  timeout
- request resolution is atomic
- archived/deleted resources remain queryable enough for audit and idempotency
- daemon restart can reconstruct active state from SQLite

Use temporary SQLite databases. Do not depend on user home directories.

### 2.3 MCP contract tests

Crate: `singleton-mcp`

Coverage:

- each default tool has stable input/output schema
- invalid inputs return machine-readable errors
- `get_capabilities` is compact and complete
- `get_inbox` returns counts plus actionable items, not transcripts
- `create_session` resolves inline workspaces through the same code path as
  `ensure_workspace`
- `send_message` is asynchronous and returns a turn id
- `read_events(wait_ms=...)` replaces a separate wait tool
- `resolve_request` handles approve, deny, respond, and cancel
- `close_resource` is idempotent

Tests should run against an in-process broker with fake backend/host.

### 2.4 Fake backend tests

Crate: `singleton-test-support`

Coverage:

- deterministic session creation/resume
- streaming message deltas
- successful turn completion
- failed turn
- cancellation
- permission request
- input request
- backend disappearance/degraded session

The fake backend is required so most broker tests do not depend on real
Copilot credentials, network, or model behavior.

### 2.5 Copilot adapter tests

Crate: `singleton-copilot`

Default tests:

- configuration validation
- backend id mapping
- provider error mapping
- event normalization for recorded/fake SDK fixtures
- permission/input handler plumbing using test doubles where possible

Ignored live tests:

- create a real Copilot session
- resume a real session
- send a message and stream events
- cancel a running turn when supported
- surface and resolve a permission request

Live tests must require an explicit feature flag and environment setup. They
must never run in the default CI gate.

### 2.6 Host/workspace tests

Crate: `singleton-host`

Coverage:

- local path workspace resolution
- git worktree creation from a local repo
- branch/base-ref metadata
- cleanup policies: keep, delete on archive, delete on success
- refusing destructive delete with active session references
- forced cleanup behavior
- cleanup idempotency
- error behavior for invalid repo/path/base ref

Use temporary git repositories created by tests.

### 2.7 CLI tests

Crate: `singleton-cli`

Coverage:

- `singleton serve` starts the broker with expected config
- `singleton status` reads broker state
- `singleton stop` requests shutdown
- CLI output is human-readable and stable enough for smoke tests

CLI tests should avoid depending on long-running external services.

---

## 3. End-to-End MVP Scenarios

These scenarios should run with fake backend and temporary local workspaces.

### 3.1 Fresh worktree session

1. Create a temporary git repo.
2. Call `create_session` with inline `git_worktree`.
3. Assert workspace and session records exist.
4. Call `send_message`.
5. Fake backend emits deltas and completion.
6. Call `read_events` until completion.
7. Close session.
8. Delete workspace explicitly.

### 3.2 Parallel fan-in

1. Create three sessions.
2. Send three turns.
3. Fake backend completes one, fails one, and requests input for one.
4. Call `get_inbox`.
5. Assert completed, failed, and input items are represented.
6. Resolve the input request.
7. Assert the resolved event is appended.

### 3.3 Resume after restart

1. Create session and turn.
2. Persist events and active state.
3. Drop broker instance.
4. Reopen broker with the same SQLite database.
5. Assert `list_sessions`, `get_session`, and `read_events` recover state.
6. Fake backend resume succeeds.

### 3.4 Backend state missing

1. Create a session with backend id mapping.
2. Restart broker.
3. Fake backend reports missing backend session.
4. Assert singleton marks the session degraded/broken.
5. Assert it does not reconstruct the backend transcript from normalized
   events.

### 3.5 Workspace cleanup safety

1. Create one workspace shared by two sessions.
2. Close one session.
3. Attempt workspace delete without force.
4. Assert delete fails because one active session remains.
5. Close second session.
6. Delete workspace.
7. Repeat delete and assert idempotent success.

---

## 4. AHP Alignment Tests

AHP is not an MVP runtime dependency, but internal state should be projectable
into AHP-like resource snapshots and action streams.

Use tests that do not import AHP crates:

- root snapshot includes hosts, capabilities, and resource links
- session snapshot includes chats, turns, status, and workspace reference
- changeset snapshot includes metadata and resource URI
- event sequences can be replayed from a cursor
- reconnect can request events after last seen sequence

If an optional AHP adapter is added later, put protocol-specific tests behind a
feature flag.

---

## 5. Legacy Python Tests

The existing Python tests document useful prior behavior but are not the target
verification gate for the Rust broker. During migration:

- keep them available for reference until equivalent Rust tests exist
- do not add new behavior to the old Python daemon/hub contracts
- do not treat old placeholder failures as blockers for doc-only design reset
  work
- remove or archive Python tests only in a dedicated cleanup change after Rust
  replacement coverage exists
