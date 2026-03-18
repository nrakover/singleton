"""Worker process management for singleton threads."""

import asyncio
import json
import os
from pathlib import Path

from singleton import hooks, store

# Default claude command
_DEFAULT_CLAUDE_CMD = "claude"

# Claude Code sets these to prevent nested sessions; strip them from subprocesses.
_CLAUDE_ENV_BLOCKLIST = ("CLAUDECODE", "CLAUDE_CODE_ENTRYPOINT")


def _subprocess_env() -> dict:
    env = os.environ.copy()
    for key in _CLAUDE_ENV_BLOCKLIST:
        env.pop(key, None)
    return env


async def spawn_worker(
    thread_id: str,
    description: str,
    context: str = "",
    cwd: str | None = None,
    permissions_mode: str = "supervised",
    state_dir: Path | None = None,
    hooks_dir: Path | None = None,
    claude_cmd: str | None = None,
) -> asyncio.subprocess.Process:
    """Spawn a worker process. Returns the process (stdin/stdout held open)."""
    if state_dir is None:
        state_dir = Path.home() / ".singleton"
    if hooks_dir is None:
        hooks_dir_env = os.environ.get("SINGLETON_HOOKS_DIR")
        if hooks_dir_env:
            hooks_dir = Path(hooks_dir_env)
        else:
            # Relative to this file: src/singleton/worker.py → project_root/hooks/
            hooks_dir = Path(__file__).parent.parent.parent / "hooks"
    if claude_cmd is None:
        claude_cmd = _DEFAULT_CLAUDE_CMD

    settings_json = hooks.generate_settings(thread_id, state_dir, hooks_dir)

    cmd = [
        claude_cmd,
        "--print",
        "--input-format=stream-json",
        "--output-format=stream-json",
        "--settings",
        settings_json,
        "--append-system-prompt",
        f"You are worker thread {thread_id}.",
    ]

    if permissions_mode == "yolo":
        cmd.append("--dangerously-skip-permissions")

    if cwd is None:
        cwd = str(state_dir / "workers" / "default")

    proc = await asyncio.create_subprocess_exec(
        *cmd,
        stdin=asyncio.subprocess.PIPE,
        stdout=asyncio.subprocess.PIPE,
        stderr=asyncio.subprocess.PIPE,
        cwd=cwd,
        env=_subprocess_env(),
    )

    # Update thread with PID
    try:
        store.update_thread(thread_id, pid=proc.pid, status="running")
    except FileNotFoundError:
        pass

    # Send initial turn with description + context
    initial_message = description
    if context:
        initial_message = f"{description}\n\nContext: {context}"

    await send_turn(proc, thread_id, initial_message, state_dir=state_dir)

    return proc


async def send_turn(
    proc: asyncio.subprocess.Process,
    thread_id: str,
    message: str,
    state_dir: Path | None = None,
) -> str:
    """Write a user turn to worker stdin, read until result event.

    Appends all output to output.txt.
    Returns result_text (<=500 chars, truncated from assistant text blocks).
    """
    if state_dir is None:
        state_dir = Path.home() / ".singleton"

    # Write user turn
    user_msg = {
        "type": "user",
        "message": {"role": "user", "content": message},
    }
    assert proc.stdin is not None
    proc.stdin.write((json.dumps(user_msg) + "\n").encode())
    await proc.stdin.drain()

    # Read until result event
    assert proc.stdout is not None
    assistant_text = ""
    raw_lines: list[str] = []

    while True:
        line_bytes = await proc.stdout.readline()
        if not line_bytes:
            break
        line = line_bytes.decode(errors="replace")
        raw_lines.append(line)

        try:
            event = json.loads(line)
        except json.JSONDecodeError:
            continue

        event_type = event.get("type", "")

        if event_type == "assistant":
            msg = event.get("message", {})
            content = msg.get("content", [])
            if isinstance(content, list):
                for block in content:
                    if isinstance(block, dict) and block.get("type") == "text":
                        assistant_text += block.get("text", "")
            elif isinstance(content, str):
                assistant_text += content

        elif event_type == "result":
            break

    # Append all output lines to output.txt
    if raw_lines:
        try:
            store.append_output(thread_id, "".join(raw_lines))
        except FileNotFoundError:
            pass

    # Truncate result text
    result_text = assistant_text[:500]
    return result_text


async def cancel_worker(proc: asyncio.subprocess.Process, thread_id: str) -> None:
    """Send SIGTERM to worker process."""
    try:
        proc.terminate()
    except ProcessLookupError:
        pass
    try:
        store.update_thread_status(thread_id, "cancelled")
    except FileNotFoundError:
        pass
