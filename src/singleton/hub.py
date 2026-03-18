"""Hub session management - runs claude in a pty."""

import asyncio
import fcntl
import json
import logging
import os
import pty
import struct
import termios
from pathlib import Path

logger = logging.getLogger(__name__)

# Claude Code sets these to prevent nested sessions; strip them from subprocesses.
_CLAUDE_ENV_BLOCKLIST = ("CLAUDECODE", "CLAUDE_CODE_ENTRYPOINT")

# Singleton MCP tools the hub is allowed to call without user confirmation.
_SINGLETON_MCP_TOOLS = [
    "create_thread",
    "list_threads",
    "get_thread",
    "thread_output",
    "get_thread_events",
    "send_to_thread",
    "cancel_thread",
    "set_thread_permissions",
    "list_pending_approvals",
    "approve_tool_call",
    "deny_tool_call",
]


def _subprocess_env() -> dict:
    env = os.environ.copy()
    for key in _CLAUDE_ENV_BLOCKLIST:
        env.pop(key, None)
    return env


class Hub:
    """Manages the hub Claude session in a pty."""

    def __init__(self, mcp_port: int, state_dir: Path):
        self.mcp_port = mcp_port
        self.state_dir = state_dir
        self.master_fd: int | None = None
        self.slave_fd: int | None = None
        self.proc: asyncio.subprocess.Process | None = None
        self._read_task: asyncio.Task | None = None

    def _setup_hub_dir(self) -> Path:
        """Create a dedicated hub working directory with MCP and settings files.

        MCP servers must be configured via .mcp.json (not settings.json).
        allowedTools lives in .claude/settings.json so the hub can call
        singleton tools without per-call confirmation prompts.
        """
        hub_dir = self.state_dir / "hub"
        claude_dir = hub_dir / ".claude"
        claude_dir.mkdir(parents=True, exist_ok=True)

        # .mcp.json — the only supported location for mcpServers in Claude Code.
        mcp_config = {
            "mcpServers": {
                "singleton": {
                    "type": "http",
                    # Use 127.0.0.1 explicitly to avoid localhost → ::1 on
                    # systems where IPv6 is preferred.
                    "url": f"http://127.0.0.1:{self.mcp_port}/mcp",
                }
            }
        }
        (hub_dir / ".mcp.json").write_text(json.dumps(mcp_config, indent=2))

        # .claude/settings.json — enable the singleton MCP server from .mcp.json
        # and pre-approve all singleton tools so the hub can call them without
        # per-call confirmation prompts.
        settings = {
            "enabledMcpjsonServers": ["singleton"],
            "permissions": {
                "allow": [f"mcp__singleton__{t}" for t in _SINGLETON_MCP_TOOLS],
            },
        }
        (claude_dir / "settings.json").write_text(json.dumps(settings, indent=2))

        return hub_dir

    async def start(self, session_id: str | None = None) -> None:
        """Start hub process in a pty."""
        self.master_fd, self.slave_fd = pty.openpty()

        hub_dir = self._setup_hub_dir()
        cmd = ["claude"]
        if session_id:
            cmd.extend(["--resume", session_id])

        self.proc = await asyncio.create_subprocess_exec(
            *cmd,
            stdin=self.slave_fd,
            stdout=self.slave_fd,
            stderr=self.slave_fd,
            cwd=str(hub_dir),
            close_fds=True,
            env=_subprocess_env(),
        )

        # Close slave fd in parent process
        os.close(self.slave_fd)
        self.slave_fd = None

        logger.info("Hub started with PID %d", self.proc.pid)

    async def stop(self) -> None:
        """Stop hub process."""
        if self._read_task is not None:
            self._read_task.cancel()
            try:
                await self._read_task
            except asyncio.CancelledError:
                pass
            self._read_task = None

        if self.proc is not None:
            try:
                self.proc.terminate()
                await asyncio.wait_for(self.proc.wait(), timeout=5.0)
            except (ProcessLookupError, asyncio.TimeoutError):
                try:
                    self.proc.kill()
                except ProcessLookupError:
                    pass
            self.proc = None

        if self.master_fd is not None:
            try:
                os.close(self.master_fd)
            except OSError:
                pass
            self.master_fd = None

        if self.slave_fd is not None:
            try:
                os.close(self.slave_fd)
            except OSError:
                pass
            self.slave_fd = None

    def write(self, data: bytes) -> None:
        """Write bytes to hub pty master fd."""
        if self.master_fd is None:
            return
        try:
            os.write(self.master_fd, data)
        except OSError as e:
            logger.warning("Hub write error: %s", e)

    async def read_loop(self, callback) -> None:
        """Read bytes from hub pty master fd and call callback(data: bytes)."""
        if self.master_fd is None:
            return

        loop = asyncio.get_event_loop()

        async def _read():
            while True:
                try:
                    data = await loop.run_in_executor(None, self._blocking_read)
                    if data:
                        callback(data)
                    else:
                        break
                except OSError:
                    break

        self._read_task = asyncio.create_task(_read())
        await self._read_task

    def _blocking_read(self) -> bytes:
        """Blocking read from pty master fd."""
        if self.master_fd is None:
            return b""
        try:
            return os.read(self.master_fd, 4096)
        except OSError:
            return b""

    def resize(self, rows: int, cols: int) -> None:
        """Resize the pty window and ensure the hub redraws.

        TIOCSWINSZ updates the kernel pty window size and delivers SIGWINCH to
        the foreground process group of the slave.  We also send SIGWINCH
        directly to the hub PID as a belt-and-suspenders measure: on re-attach
        the hub may not be the current foreground process group leader, so the
        kernel delivery alone is unreliable.
        """
        if self.master_fd is None:
            return
        try:
            size = struct.pack("HHHH", rows, cols, 0, 0)
            fcntl.ioctl(self.master_fd, termios.TIOCSWINSZ, size)
        except OSError as e:
            logger.warning("Hub resize error: %s", e)
            return
        if self.proc is not None:
            try:
                import signal

                os.kill(self.proc.pid, signal.SIGWINCH)
            except (ProcessLookupError, OSError):
                pass
