# singleton — Development Guide for Claude

## Verification Commands

Run all three before marking any implementation task complete:

```bash
uv run pytest                 # unit + integration tests (must be 0 failures)
uv run ruff format .          # auto-format (run before committing)
uv run ruff check src/ tests/ # lint (must be clean)
uv run ty check               # type checking (must be 0 errors)
```

All three must pass simultaneously. Do not ignore failures in one to fix another.

## Project Structure

- `src/singleton/` — main package
- `tests/` — pytest test suite (`asyncio_mode = "auto"`)
- `hooks/` — shell hook scripts for workers
- `spec/` — canonical behavioral spec (keep in sync with code)
- `project_tasks/` — per-task implementation plans

## Key Constraints

- **MCP servers** must be configured in `.mcp.json`, NOT `settings.json` (any scope). Use `enabledMcpjsonServers` + `permissions.allow` in `.claude/settings.json`. See `spec/spec.md §2.2`.
- **Hub** runs from `~/.singleton/hub/` (CWD) so Claude Code picks up `.mcp.json` naturally.
- **Env vars** `CLAUDECODE` and `CLAUDE_CODE_ENTRYPOINT` must be stripped before spawning hub or worker subprocesses (`_subprocess_env()` in `hub.py` and `worker.py`).
- **MCP tool functions** that call `asyncio.create_task` must be `async` — FastMCP runs sync tools in a thread executor where no event loop is accessible.

## Spec Discipline

When changing behavior, update **all** of:
1. `spec/spec.md`
2. `spec/interfaces.md` (if interface contract changes)
3. `project_tasks/0_bootstrap.md` (implementation details section)
4. `MEMORY.md` in the Claude project memory dir
