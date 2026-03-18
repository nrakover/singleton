"""Singleton daemon - central asyncio coordinator."""

import asyncio
import base64
import json
import logging
import os
import signal
from pathlib import Path

from singleton import mcp_server, store, worker
from singleton.hub import Hub

logger = logging.getLogger(__name__)

SOCKET_PROTOCOL_VERSION = 1


class WorkerManager:
    """Manages worker processes on behalf of MCP tools."""

    def __init__(self, daemon: "Daemon"):
        self._daemon = daemon

    async def spawn(self, thread_id: str) -> None:
        """Spawn a worker for an existing thread."""
        await self._daemon.spawn_thread_worker(thread_id)

    async def send(self, thread_id: str, message: str) -> str:
        """Send a message to a worker, return result text."""
        return await self._daemon.send_to_worker(thread_id, message)

    async def cancel(self, thread_id: str) -> None:
        """Cancel a worker."""
        await self._daemon.cancel_worker(thread_id)


class Daemon:
    """Central asyncio coordinator for singleton."""

    def __init__(self, mcp_port: int = 32100, state_dir: Path | None = None):
        self.mcp_port = mcp_port
        self.state_dir = state_dir or (Path.home() / ".singleton")
        self.hub: Hub | None = None
        self.workers: dict[str, asyncio.subprocess.Process] = {}
        self.hub_busy: bool = False
        self.injection_queue: asyncio.Queue = asyncio.Queue(maxsize=10)
        self.cli_connections: list[asyncio.StreamWriter] = []
        self._hub_quiet_task: asyncio.Task | None = None
        self._running = False
        self._socket_server: asyncio.Server | None = None
        self._hooks_dir = Path(__file__).parent.parent.parent / "hooks"

    async def start(self) -> None:
        """Start daemon: init dirs, MCP, hub, file watcher, socket."""
        store.SINGLETON_DIR = self.state_dir
        store.THREADS_DIR = self.state_dir / "threads"
        store.init_dirs()

        # Write PID
        store.write_daemon_pid(os.getpid())
        store.write_mcp_port(self.mcp_port)

        # Set up worker manager
        manager = WorkerManager(self)
        mcp_server.set_worker_manager(manager)

        # Crash recovery
        await self._crash_recovery()

        self._running = True

        # Bind Unix socket first so the CLI can connect as soon as we signal ready.
        sock_path = self.state_dir / "daemon.sock"
        if sock_path.exists():
            sock_path.unlink()
        self._socket_server = await asyncio.start_unix_server(
            self._handle_cli_connection,
            path=str(sock_path),
        )

        # Start MCP HTTP server in the background, then wait until it is
        # actually accepting connections before launching the hub.
        asyncio.create_task(self._run_mcp_server())
        await self._wait_for_mcp_ready()

        # Start hub (MCP server is now listening, so the hub can connect).
        self.hub = Hub(mcp_port=self.mcp_port, state_dir=self.state_dir)
        hub_session_id = store.read_hub_session_id()
        await self.hub.start(session_id=hub_session_id)

        asyncio.create_task(self._file_watcher_loop())
        asyncio.create_task(self._injection_loop())
        asyncio.create_task(self._hub_read_loop())

        logger.info("Daemon started on port %d, socket %s", self.mcp_port, sock_path)

    async def _wait_for_mcp_ready(self, timeout: float = 10.0) -> None:
        """Poll until the MCP HTTP server is accepting TCP connections."""
        deadline = asyncio.get_event_loop().time() + timeout
        while asyncio.get_event_loop().time() < deadline:
            try:
                _, writer = await asyncio.open_connection("127.0.0.1", self.mcp_port)
                writer.close()
                await writer.wait_closed()
                logger.info("MCP server ready on port %d", self.mcp_port)
                return
            except OSError:
                await asyncio.sleep(0.1)
        raise RuntimeError(
            f"MCP server did not become ready within {timeout}s on port {self.mcp_port}"
        )

    async def _run_mcp_server(self) -> None:
        """Run the MCP HTTP server."""
        try:
            await mcp_server.mcp.run_async(
                transport="streamable-http",
                host="127.0.0.1",
                port=self.mcp_port,
            )
        except Exception as e:
            logger.error("MCP server error: %s", e)

    async def stop(self) -> None:
        """Graceful shutdown."""
        self._running = False

        # Stop hub
        if self.hub is not None:
            await self.hub.stop()

        # Terminate workers
        for thread_id, proc in list(self.workers.items()):
            try:
                proc.terminate()
                await asyncio.wait_for(proc.wait(), timeout=3.0)
            except (ProcessLookupError, asyncio.TimeoutError):
                try:
                    proc.kill()
                except ProcessLookupError:
                    pass

        # Close socket server
        if self._socket_server is not None:
            self._socket_server.close()
            await self._socket_server.wait_closed()

        # Remove socket file
        sock_path = self.state_dir / "daemon.sock"
        if sock_path.exists():
            sock_path.unlink()

        # Remove PID file
        store.remove_daemon_pid()

        logger.info("Daemon stopped")

    async def spawn_thread_worker(self, thread_id: str) -> None:
        """Spawn worker for an existing thread."""
        thread = store.get_thread(thread_id)
        proc = await worker.spawn_worker(
            thread_id=thread_id,
            description=thread["description"],
            context=thread.get("context", ""),
            cwd=thread.get("cwd"),
            permissions_mode=thread.get("permissions_mode", "supervised"),
            state_dir=self.state_dir,
            hooks_dir=self._hooks_dir,
        )
        self.workers[thread_id] = proc
        store.update_thread(thread_id, pid=proc.pid, status="idle")

    async def send_to_worker(self, thread_id: str, message: str) -> str:
        """Send message to worker, return result_text."""
        proc = self.workers.get(thread_id)
        if proc is None:
            raise RuntimeError(f"No running worker for thread {thread_id}")
        store.update_thread_status(thread_id, "running")
        result = await worker.send_turn(
            proc, thread_id, message, state_dir=self.state_dir
        )
        store.update_thread_status(thread_id, "idle")
        return result

    async def cancel_worker(self, thread_id: str) -> None:
        """SIGTERM worker, update status."""
        proc = self.workers.pop(thread_id, None)
        if proc is not None:
            await worker.cancel_worker(proc, thread_id)

    async def _crash_recovery(self) -> None:
        """Check threads for orphaned running/idle processes."""
        threads_dir = self.state_dir / "threads"
        if not threads_dir.exists():
            return

        for thread_dir in threads_dir.iterdir():
            if not thread_dir.is_dir():
                continue
            try:
                t = store.get_thread(thread_dir.name)
            except (FileNotFoundError, Exception):
                continue

            if t["status"] in ("running", "idle", "awaiting_approval"):
                pid = t.get("pid")
                if pid and self._pid_alive(pid):
                    store.update_thread_status(t["id"], "disconnected")
                else:
                    store.update_thread_status(t["id"], "done")

    def _pid_alive(self, pid: int) -> bool:
        """Check if a PID is still running."""
        try:
            os.kill(pid, 0)
            return True
        except (OSError, ProcessLookupError):
            return False

    async def _file_watcher_loop(self) -> None:
        """Watch threads/*/events/ for new files using watchfiles."""
        from watchfiles import awatch

        threads_dir = self.state_dir / "threads"
        if not threads_dir.exists():
            return

        try:
            stop_ev = self._stop_event()
            async for changes in awatch(str(threads_dir), stop_event=stop_ev):
                for _change_type, path in changes:
                    if "events" in path and path.endswith(".json"):
                        await self._handle_event(Path(path))
        except Exception as e:
            if self._running:
                logger.error("File watcher error: %s", e)

    def _stop_event(self):
        """Return an asyncio.Event that gets set when daemon stops."""
        ev = asyncio.Event()

        async def _wait_for_stop():
            while self._running:
                await asyncio.sleep(0.5)
            ev.set()

        asyncio.create_task(_wait_for_stop())
        return ev

    async def _handle_event(self, event_path: Path) -> None:
        """Process a new event file (stop, pretool, notification)."""
        try:
            event = json.loads(event_path.read_text())
        except (json.JSONDecodeError, FileNotFoundError):
            return

        event_type = event.get("type", "")
        thread_id = event.get("thread_id", "")

        if event_type == "stop":
            session_id = event.get("data", {}).get("session_id", "")
            if session_id:
                store.update_thread(thread_id, session_id=session_id)
            store.update_thread_status(thread_id, "idle")

            try:
                t = store.get_thread(thread_id)
                summary = t.get("last_turn_summary", "")
            except FileNotFoundError:
                summary = ""

            inject_text = (
                f"\n[Thread {thread_id} completed. Summary: {summary[:200]}]\n"
            )
            await self._enqueue_injection(inject_text)

        elif event_type == "pretool":
            data = event.get("data", {})
            mode = data.get("mode", "supervised")
            req_id = data.get("request_id", "")
            tool = data.get("tool", "")
            inp = data.get("input", {})

            if mode == "supervised":
                inject_text = (
                    f"\n[Thread {thread_id} requests approval: "
                    f"{tool}({json.dumps(inp)[:100]}). "
                    f"Request ID: {req_id}. "
                    f"Use approve_tool_call or deny_tool_call.]\n"
                )
                store.update_thread_status(thread_id, "awaiting_approval")
                await self._enqueue_injection(inject_text)

            elif mode == "passthrough":
                # Send passthrough_prompt to all CLI connections
                msg = {
                    "type": "passthrough_prompt",
                    "thread_id": thread_id,
                    "request_id": req_id,
                    "tool": tool,
                    "input": inp,
                }
                self._broadcast_to_cli(json.dumps(msg).encode() + b"\n")
                store.update_thread_status(thread_id, "awaiting_approval")

        elif event_type == "notification":
            message = event.get("data", {}).get("message", "")
            logger.info("Thread %s notification: %s", thread_id, message)

    async def _enqueue_injection(self, text: str) -> None:
        """Queue an injection into the hub pty."""
        try:
            self.injection_queue.put_nowait(text)
        except asyncio.QueueFull:
            logger.warning("Injection queue full, dropping message")

    async def _injection_loop(self) -> None:
        """Drain injection queue when hub not busy."""
        while self._running:
            text = await self.injection_queue.get()
            # Wait until hub is not busy
            while self.hub_busy:
                await asyncio.sleep(0.05)
            await self._inject(text)
            self.injection_queue.task_done()

    async def _inject(self, text: str) -> None:
        """Write text to hub pty master fd."""
        if self.hub is not None:
            self.hub.write(text.encode())

    def _on_hub_output(self, data: bytes) -> None:
        """Called when hub pty produces output."""
        self.hub_busy = True

        # Cancel existing quiet timer
        if self._hub_quiet_task is not None:
            self._hub_quiet_task.cancel()

        # Schedule reset after 200ms of silence
        loop = asyncio.get_event_loop()
        self._hub_quiet_task = loop.create_task(self._hub_quiet_timer())

        # Fan out to CLI connections
        self._fan_out_hub_output(data)

    async def _hub_quiet_timer(self) -> None:
        """Reset hub_busy after 200ms of quiet."""
        await asyncio.sleep(0.2)
        self.hub_busy = False

    async def _hub_read_loop(self) -> None:
        """Read hub pty output and fan out to CLI connections."""
        if self.hub is not None:
            await self.hub.read_loop(self._on_hub_output)
        # Read loop ended — hub process has exited.
        await self._on_hub_exit()

    async def _on_hub_exit(self) -> None:
        """Handle hub process exit: notify CLIs, clean up, leave daemon running."""
        if not self._running:
            return  # Daemon is shutting down — normal path, skip notification.

        logger.info("Hub session ended")

        if self.hub is not None:
            # Wait briefly for the process to fully exit then capture exit code.
            try:
                if self.hub.proc is not None:
                    await asyncio.wait_for(self.hub.proc.wait(), timeout=2.0)
            except asyncio.TimeoutError:
                pass
            await self.hub.stop()
            self.hub = None

        # Notify attached CLI connections so they can exit cleanly.
        msg = (
            json.dumps(
                {
                    "type": "hub_exit",
                    "message": (
                        "Hub session ended. Run 'singleton' to start a new session."
                    ),
                }
            )
            + "\n"
        ).encode()
        for writer in list(self.cli_connections):
            try:
                writer.write(msg)
                await writer.drain()
            except Exception:
                pass

    def _fan_out_hub_output(self, data: bytes) -> None:
        """Send hub pty output to all attached CLI connections."""
        encoded = base64.b64encode(data).decode()
        msg = json.dumps({"type": "output", "data": encoded}) + "\n"
        msg_bytes = msg.encode()
        dead = []
        for writer in self.cli_connections:
            try:
                writer.write(msg_bytes)
            except Exception:
                dead.append(writer)
        for w in dead:
            self.cli_connections.remove(w)

    def _broadcast_to_cli(self, data: bytes) -> None:
        """Send raw bytes to all CLI connections."""
        dead = []
        for writer in self.cli_connections:
            try:
                writer.write(data)
            except Exception:
                dead.append(writer)
        for w in dead:
            self.cli_connections.remove(w)

    async def _socket_server_loop(self) -> None:
        """Serve CLI connections via Unix socket."""
        sock_path = self.state_dir / "daemon.sock"
        server = await asyncio.start_unix_server(
            self._handle_cli_connection,
            path=str(sock_path),
        )
        async with server:
            await server.serve_forever()

    async def _handle_cli_connection(
        self, reader: asyncio.StreamReader, writer: asyncio.StreamWriter
    ) -> None:
        """Handle a single CLI socket connection."""
        self.cli_connections.append(writer)
        try:
            while True:
                line = await reader.readline()
                if not line:
                    break
                try:
                    msg = json.loads(line.decode().strip())
                except json.JSONDecodeError:
                    continue

                msg_type = msg.get("type", "")

                if msg_type == "attach":
                    rows = msg.get("tty_rows", 24)
                    cols = msg.get("tty_cols", 80)
                    if self.hub is None:
                        # Hub exited previously — restart it.
                        session_id = store.read_hub_session_id()
                        self.hub = Hub(mcp_port=self.mcp_port, state_dir=self.state_dir)
                        await self.hub.start(session_id=session_id)
                        asyncio.create_task(self._hub_read_loop())
                    self.hub.resize(rows, cols)
                    ack = json.dumps({"type": "ack"}) + "\n"
                    writer.write(ack.encode())
                    await writer.drain()

                elif msg_type == "input":
                    raw = base64.b64decode(msg.get("data", ""))
                    if self.hub is not None:
                        self.hub.write(raw)

                elif msg_type == "resize":
                    rows = msg.get("rows", 24)
                    cols = msg.get("cols", 80)
                    if self.hub is not None:
                        self.hub.resize(rows, cols)

                elif msg_type == "detach":
                    break

                elif msg_type == "status_request":
                    threads = store.list_threads()
                    pending = store.list_pending_approvals()
                    resp = (
                        json.dumps(
                            {
                                "type": "status_response",
                                "threads": threads,
                                "pending_approvals": pending,
                            }
                        )
                        + "\n"
                    )
                    writer.write(resp.encode())
                    await writer.drain()

                elif msg_type == "stop_request":
                    # Stop daemon
                    asyncio.create_task(self.stop())
                    break

        finally:
            if writer in self.cli_connections:
                self.cli_connections.remove(writer)
            try:
                writer.close()
                await writer.wait_closed()
            except Exception:
                pass


def _configure_logging(state_dir: Path) -> None:
    """Set up logging to a per-run file under state_dir/logs/."""
    from datetime import datetime

    logs_dir = state_dir / "logs"
    logs_dir.mkdir(parents=True, exist_ok=True)

    timestamp = datetime.now().strftime("%Y%m%dT%H%M%S")
    log_path = logs_dir / f"{timestamp}_{os.getpid()}.daemon.log"

    logging.basicConfig(
        level=logging.INFO,
        format="%(asctime)s %(levelname)s %(name)s: %(message)s",
        handlers=[logging.FileHandler(log_path)],
    )
    # Print the log path so the user knows where to look.
    print(f"Daemon logging to {log_path}", flush=True)


async def run_daemon(mcp_port: int = 32100, state_dir: Path | None = None) -> None:
    """Entry point for running the daemon."""
    resolved_state_dir = state_dir or (Path.home() / ".singleton")
    resolved_state_dir.mkdir(parents=True, exist_ok=True)
    _configure_logging(resolved_state_dir)
    daemon = Daemon(mcp_port=mcp_port, state_dir=resolved_state_dir)

    loop = asyncio.get_event_loop()

    def _handle_sigterm():
        asyncio.create_task(daemon.stop())

    loop.add_signal_handler(signal.SIGTERM, _handle_sigterm)
    loop.add_signal_handler(signal.SIGINT, _handle_sigterm)

    await daemon.start()

    # Keep running
    while daemon._running:
        await asyncio.sleep(1)


if __name__ == "__main__":
    asyncio.run(run_daemon())
