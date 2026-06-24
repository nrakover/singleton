# singleton - Test Strategy

## 1. Scope

The current behavioral target is the Rust MCP session broker. The previous
Python pytest suite has been removed with the legacy prototype.

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
- `ack_inbox` idempotently marks unread completed/failed turn items as read
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
- send a message and stream events
- drive `singleton serve --backend copilot --stdio` over MCP initialize,
  create-session, send-message, and read-events
- resume a real session
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

- `singleton serve --backend fake --stdio` starts a JSON-RPC MCP server
- default stdio mode proxies to a daemon so proxy disconnect does not stop
  broker-owned turns
- `singleton serve --backend copilot` selects the Copilot backend
- ignored live test validates `singleton serve --backend copilot --stdio`
  through the MCP wire protocol
- `singleton status` reads broker state
- `singleton start`, `singleton status`, and `singleton stop` manage pid/socket
  daemon lifecycle
- `singleton mcp-config --backend copilot` prints an MCP server config snippet
- `singleton install-mcp --client copilot|claude|codex --dry-run` builds the
  expected native MCP registration commands
- stdio `initialize`, `tools/list`, and `tools/call` work against the fake
  backend for a create/send/read-events vertical slice
- CLI output is human-readable and stable enough for smoke tests

CLI tests should avoid depending on long-running external services.

### 2.8 Packaging and plugin tests

Coverage:

- release workflow builds `singleton` for supported macOS/Linux targets and
  publishes `.tar.gz` archives plus `.sha256` files on `v*.*.*` tags
- release archives contain an executable `singleton` binary
- Copilot marketplace manifest points at the plugin subdirectory
- Copilot plugin manifest points to the MCP config file
- Copilot plugin manifest points to the `skills` directory
- plugin `skills/singleton/SKILL.md` exists with valid Skill frontmatter
- plugin MCP config starts the launcher through `bash`
- plugin launcher shell script passes syntax checks
- plugin launcher writes bootstrap diagnostics to stderr, not stdout
- plugin launcher supports `SINGLETON_BINARY`, `SINGLETON_VERSION`,
  `SINGLETON_RELEASE_BASE_URL`, `SINGLETON_FORCE_INSTALL`,
  `SINGLETON_BACKEND`, and `SINGLETON_DATABASE`
- local `copilot plugin marketplace add PATH` plus
  `copilot plugin install singleton@singleton` succeeds when Copilot CLI is
  available

Packaging tests should not download release assets in the default unit test
gate. Networked release/download checks belong in release or manual smoke
validation.

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
7. Call `get_inbox` and `ack_inbox` for the unread completion.
8. Close session.
9. Delete workspace explicitly.

### 3.2 Parallel fan-in

1. Create three sessions.
2. Send three turns.
3. Fake backend completes one, fails one, and requests input for one.
4. Call `get_inbox`.
5. Assert completed, failed, and input items are represented.
6. Resolve the input request.
7. Assert the resolved event is appended.
8. Cancel a needs-input turn and assert pending requests are cancelled.

### 3.3 Resume after restart

1. Create session and turn.
2. Persist events and active state.
3. Drop broker instance.
4. Reopen broker with the same SQLite database.
5. Assert persisted backend sessions are resumed when the backend supports
   resume.
6. Assert active turns are reattached when the backend supports active-turn
   reattach.
7. Assert active turns are marked failed/unread with an interrupted/retryable
   event when the backend cannot reattach the turn, and pending requests for
   that turn are cancelled.

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

### 3.6 Copilot plugin smoke

1. Build or install a local singleton binary.
2. Run `copilot plugin marketplace add PATH_TO_CLEAN_REPO`.
3. Run `copilot plugin install singleton@singleton`.
4. Start a new Copilot CLI session.
5. Verify the `singleton` MCP tools are listed.
6. Verify the plugin-packaged `singleton` Skill is available when the CLI exposes
   skill inventory.
7. Call `get_capabilities`, `create_session`, `send_message`, and
   `read_events`.
8. Uninstall the local plugin.

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
