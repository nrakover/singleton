# singleton - Agent Development Guide

## Current Direction

`singleton` is a Rust-based MCP session broker for durable background agent
sessions. Historical Python/Claude planning docs may remain for context, but
the executable implementation is the Rust/Copilot broker.

Primary references:

- `spec/spec.md`
- `spec/interfaces.md`
- `spec/user_flows.md`
- `spec/tests.md`
- `project_tasks/2_agent-session-mcp-pivot.md`

Copilot CLI/app is the working environment going forward. Claude Code artifacts
are not authoritative and should not be reintroduced unless a task explicitly
asks for legacy archaeology.

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

Documentation-only design reset work does not require running either gate.

## Project Structure

Current:

- `spec/` - canonical current behavioral spec
- `project_tasks/` - implementation plans and backlog
- `crates/singleton-core`
- `crates/singleton-store`
- `crates/singleton-copilot`
- `crates/singleton-mcp`
- `crates/singleton-host`
- `crates/singleton-cli`
- `crates/singleton-broker`
- `crates/singleton-test-support`

## Key Constraints

- The MVP is Copilot-only internally through the GitHub Copilot SDK, with a
  deterministic fake backend for tests.
- The default MCP tool surface must stay compact:
  `get_capabilities`, `get_inbox`, `ack_inbox`, `ensure_workspace`,
  `create_session`, `send_message`, `read_events`, `get_latest_output`,
  `list_sessions`, `get_session`, `resolve_request`, `cancel_turn`,
  `close_resource`.
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

Do not recreate or extend the superseded Python hub/worker architecture unless
the task is explicitly scoped as legacy archaeology.

## Test Catalogue Discipline

`spec/tests.md` is the PL-agnostic catalogue of hard invariants enforced by the
executable test suite. Every entry must include preconditions, postconditions,
and invariants, plus a status that distinguishes enforced coverage from planned
or historical targets.

When adding or changing executable tests, update `spec/tests.md` in the same
change so the entry maps directly to formal assertions. If a spec entry has no
corresponding executable test, mark it planned/unimplemented rather than
implying it is enforced.
