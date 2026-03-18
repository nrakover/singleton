"""Singleton CLI - terminal relay to hub pty."""

import argparse
import asyncio
import base64
import json
import os
import subprocess
import sys
import termios
import tty

from singleton import store

PREFIX_KEY = b"\x02"  # Ctrl+b


def main() -> None:
    parser = argparse.ArgumentParser(prog="singleton")
    subparsers = parser.add_subparsers(dest="command")
    subparsers.add_parser("attach", help="Attach to hub pty")
    subparsers.add_parser("status", help="Print thread status")
    subparsers.add_parser("stop", help="Stop daemon")
    # Hidden daemon subcommand
    daemon_parser = subparsers.add_parser("daemon", help=argparse.SUPPRESS)
    daemon_parser.add_argument("--port", type=int, default=32100)
    args = parser.parse_args()

    command = args.command or "attach"

    if command == "attach":
        asyncio.run(_cmd_attach())
    elif command == "status":
        asyncio.run(_cmd_status())
    elif command == "stop":
        _cmd_stop()
    elif command == "daemon":
        port = getattr(args, "port", 32100)
        asyncio.run(_run_daemon(port))


async def _run_daemon(port: int) -> None:
    """Run daemon process."""
    from singleton.daemon import run_daemon

    await run_daemon(mcp_port=port)


async def _try_connect(sock_path) -> "tuple | None":
    """Attempt a single connection to the daemon socket. Returns None on failure."""
    try:
        return await asyncio.open_unix_connection(str(sock_path))
    except (ConnectionRefusedError, FileNotFoundError, OSError):
        return None


async def _cmd_attach() -> None:
    """Attach to hub pty. Start daemon first if not running."""
    sock_path = store.SINGLETON_DIR / "daemon.sock"

    # Probe with a real connection attempt — a stale socket file that nobody
    # is listening on will give ECONNREFUSED, same as a missing file.
    conn = await _try_connect(sock_path)

    if conn is None:
        # Remove any stale socket file before starting a fresh daemon.
        if sock_path.exists():
            sock_path.unlink()
        await _start_daemon_background()
        # Poll until we get a successful connection (not just file existence).
        for _ in range(100):
            conn = await _try_connect(sock_path)
            if conn is not None:
                break
            await asyncio.sleep(0.1)

    if conn is None:
        print("Error: daemon did not start within 10 seconds", file=sys.stderr)
        sys.exit(1)

    reader, writer = conn

    rows, cols = _get_terminal_size()
    _send_msg(writer, {"type": "attach", "tty_rows": rows, "tty_cols": cols})

    # Enter raw mode
    fd = sys.stdin.fileno()
    try:
        old_settings = termios.tcgetattr(fd)
        tty.setraw(fd)
    except termios.error:
        old_settings = None

    try:
        await _relay_loop(reader, writer)
    finally:
        if old_settings is not None:
            try:
                termios.tcsetattr(fd, termios.TCSADRAIN, old_settings)
            except termios.error:
                pass
        writer.close()
        try:
            await writer.wait_closed()
        except Exception:
            pass


async def _relay_loop(
    reader: asyncio.StreamReader, writer: asyncio.StreamWriter
) -> None:
    """Relay between stdin/stdout and daemon socket. Handle prefix key."""
    loop = asyncio.get_event_loop()

    # State: NORMAL or COMMAND
    prefix_pending = False
    stdin_fd = sys.stdin.fileno()

    async def read_stdin() -> None:
        """Read raw bytes from stdin using add_reader so the fd registration
        is properly removed on task cancellation (unlike run_in_executor whose
        underlying thread cannot be cancelled and becomes a zombie that
        competes with the next attach for stdin bytes)."""
        nonlocal prefix_pending
        while True:
            fut: asyncio.Future[bytes] = loop.create_future()

            def _stdin_ready() -> None:
                # Called by the event loop when stdin is readable.
                # Remove the reader before touching the future so a second
                # call can never race.
                loop.remove_reader(stdin_fd)
                if fut.done():
                    return
                try:
                    fut.set_result(os.read(stdin_fd, 256))
                except OSError as exc:
                    fut.set_exception(exc)

            loop.add_reader(stdin_fd, _stdin_ready)
            try:
                data = await fut
            except (OSError, asyncio.CancelledError):
                loop.remove_reader(stdin_fd)
                return
            if not data:
                loop.remove_reader(stdin_fd)
                return

            if prefix_pending:
                prefix_pending = False
                if data == b"d":
                    _send_msg(writer, {"type": "detach"})
                    await writer.drain()
                    return
                elif data == b"?":
                    _print_help()
                    continue
                else:
                    combined = PREFIX_KEY + data
                    _send_msg(
                        writer,
                        {
                            "type": "input",
                            "data": base64.b64encode(combined).decode(),
                        },
                    )
                    await writer.drain()
            elif data == PREFIX_KEY:
                prefix_pending = True
            else:
                _send_msg(
                    writer,
                    {
                        "type": "input",
                        "data": base64.b64encode(data).decode(),
                    },
                )
                await writer.drain()

    async def read_daemon() -> None:
        while True:
            try:
                line = await reader.readline()
            except (asyncio.IncompleteReadError, OSError):
                break
            if not line:
                break
            try:
                msg = json.loads(line.decode().strip())
            except json.JSONDecodeError:
                continue

            if msg.get("type") == "output":
                raw = base64.b64decode(msg.get("data", ""))
                sys.stdout.buffer.write(raw)
                sys.stdout.buffer.flush()
            elif msg.get("type") == "hub_exit":
                message = msg.get("message", "Hub session ended.")
                sys.stdout.buffer.write(f"\r\n{message}\r\n".encode())
                sys.stdout.buffer.flush()
                return  # Exit relay loop; CLI will restore terminal and exit.
            elif msg.get("type") == "passthrough_prompt":
                _handle_passthrough_prompt(msg, writer)

    stdin_task = asyncio.create_task(read_stdin())
    daemon_task = asyncio.create_task(read_daemon())

    done, pending = await asyncio.wait(
        [stdin_task, daemon_task],
        return_when=asyncio.FIRST_COMPLETED,
    )
    for task in pending:
        task.cancel()
        try:
            await task
        except (asyncio.CancelledError, Exception):
            pass


def _handle_passthrough_prompt(msg: dict, writer: asyncio.StreamWriter) -> None:
    """Handle a passthrough approval prompt."""
    thread_id = msg.get("thread_id", "")
    req_id = msg.get("request_id", "")
    tool = msg.get("tool", "")
    inp = msg.get("input", {})

    print(
        f"\r\n[PASSTHROUGH] Thread {thread_id} requests {tool}({json.dumps(inp)[:80]})",
        file=sys.stderr,
    )
    print(r"\r\nApprove? [y/N]: ", end="", file=sys.stderr, flush=True)
    try:
        answer = sys.stdin.readline().strip().lower()
        decision = "approve" if answer in ("y", "yes") else "deny"
    except OSError:
        decision = "deny"

    from singleton import store as _store

    try:
        _store.write_response(req_id, thread_id, decision, "user")
    except Exception:
        pass


def _print_help() -> None:
    """Print prefix key help."""
    print("\r\n[singleton] Prefix key: Ctrl+b", file=sys.stderr)
    print("  d - detach from hub", file=sys.stderr)
    print("  ? - show this help", file=sys.stderr, flush=True)


def _get_terminal_size() -> tuple[int, int]:
    """Return (rows, cols) of terminal."""
    try:
        cols, rows = os.get_terminal_size()
        return rows, cols
    except OSError:
        return 24, 80


def _send_msg(writer: asyncio.StreamWriter, msg: dict) -> None:
    """Send a JSON message to daemon."""
    writer.write((json.dumps(msg) + "\n").encode())


async def _cmd_status() -> None:
    """Request and print status board."""
    sock_path = store.SINGLETON_DIR / "daemon.sock"
    conn = await _try_connect(sock_path)
    if conn is None:
        print("Daemon not running")
        return

    reader, writer = conn
    _send_msg(writer, {"type": "status_request"})
    await writer.drain()

    line = await reader.readline()
    if line:
        try:
            resp = json.loads(line.decode().strip())
            _print_status_board(resp)
        except json.JSONDecodeError:
            print("Invalid response from daemon")

    writer.close()
    try:
        await writer.wait_closed()
    except Exception:
        pass


def _print_status_board(resp: dict) -> None:
    """Print a formatted status board."""
    threads = resp.get("threads", [])
    pending = resp.get("pending_approvals", [])

    print(f"{'ID':<8} {'STATUS':<16} {'PERMISSIONS':<14} DESCRIPTION")
    print("-" * 70)
    for t in threads:
        tid = t.get("id", "")[:8]
        status = t.get("status", "")
        mode = t.get("permissions_mode", "")
        desc = t.get("description", "")[:40]
        print(f"{tid:<8} {status:<16} {mode:<14} {desc}")

    if pending:
        print("\nPENDING APPROVALS:")
        for req in pending:
            print(
                f"  [{req.get('request_id', '')}] "
                f"Thread {req.get('thread_id', '')}: "
                f"{req.get('tool', '')} - {json.dumps(req.get('input', {}))[:60]}"
            )


def _cmd_stop() -> None:
    """Stop daemon by sending SIGTERM to PID."""
    pid = store.read_daemon_pid()
    if pid is None:
        print("Daemon not running")
        return
    try:
        import signal

        os.kill(pid, signal.SIGTERM)
        print(f"Sent SIGTERM to daemon (PID {pid})")
    except ProcessLookupError:
        print(f"No process found with PID {pid}")
        store.remove_daemon_pid()


async def _start_daemon_background() -> None:
    """Start daemon process in background."""
    singleton_bin = sys.argv[0]
    subprocess.Popen(
        [singleton_bin, "daemon"],
        start_new_session=True,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    # Give it a moment to start
    await asyncio.sleep(0.5)
