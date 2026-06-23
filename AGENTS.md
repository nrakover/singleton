# singleton - Agent Development Guide

## Current Direction

`singleton` is being reset into a Rust-based MCP session broker for durable
background agent sessions. The previous Python/Claude hub, worker, hook, and
TUI architecture is historical reference unless a task explicitly asks for
legacy maintenance.

Primary references:

- `spec/spec.md`
- `spec/interfaces.md`
- `spec/user_flows.md`
- `spec/tests.md`
- `project_tasks/2_agent-session-mcp-pivot.md`

Copilot CLI/app is the working environment going forward. Legacy Claude Code
artifacts may be useful for historical context, but they are not authoritative
and should not be kept in sync.

## Verification Commands

For Rust implementation work, run all before marking a task complete:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo test --workspace --all-targets
```

Live Copilot-backed tests are opt-in:

```bash
cargo test --workspace --features live-copilot -- --ignored
```

While legacy Python code remains in the repository, use the old Python gate
only for tasks that intentionally modify that code:

```bash
uv run pytest
uv run ruff format .
uv run ruff check src/ tests/
uv run ty check
```

Documentation-only design reset work does not require running either gate.

## Project Structure

Current/historical:

- `src/singleton/` - legacy Python prototype
- `tests/` - legacy pytest suite
- `hooks/` - legacy Claude hook scripts
- `spec/` - canonical current behavioral spec
- `project_tasks/` - implementation plans and backlog

Planned Rust workspace:

- `crates/singleton-core`
- `crates/singleton-store`
- `crates/singleton-copilot`
- `crates/singleton-mcp`
- `crates/singleton-host`
- `crates/singleton-cli`
- `crates/singleton-test-support`

## Key Constraints

- The MVP is Copilot-only internally through the GitHub Copilot SDK, with a
  deterministic fake backend for tests.
- The default MCP tool surface must stay compact:
  `get_capabilities`, `get_inbox`, `ack_inbox`, `ensure_workspace`,
  `create_session`, `send_message`, `read_events`, `list_sessions`,
  `get_session`, `resolve_request`, `cancel_turn`, `close_resource`.
- A foreground agent acts as the "hub" by convention. Singleton does not own a
  foreground hub session or TUI in the new architecture.
- Host, workspace, session, chat, turn, request, changeset, terminal, and
  artifact identities must remain separate.
- Store stable resource URIs and sequence-numbered events from day one so AHP
  adapters can be added later.
- AHP is a future alignment/adapter target, not an MVP runtime dependency.
- Singleton owns orchestration state and workspace lifecycle; the Copilot SDK
  owns canonical conversation persistence; the filesystem owns source files
  and git state.
- Secrets and host credentials must be references only; do not store raw secret
  material in SQLite.

## Spec Discipline

When changing behavior, update all relevant current artifacts:

1. `spec/spec.md`
2. `spec/interfaces.md` if interface contracts change
3. `spec/user_flows.md` if user-visible flows change
4. `spec/tests.md` if validation strategy or expected behavior changes
5. `project_tasks/2_agent-session-mcp-pivot.md` or `project_tasks/backlog.md`

Do not extend the superseded Python hub/worker architecture unless the task is
explicitly scoped as legacy maintenance.
