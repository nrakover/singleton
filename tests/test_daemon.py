"""Tests for singleton.daemon module (T-DAEMON-1 through T-DAEMON-12)."""

import asyncio
import base64
import json
import os
from pathlib import Path
from unittest.mock import AsyncMock, MagicMock, patch

import pytest

import singleton.store as store
from singleton.daemon import Daemon, WorkerManager


@pytest.fixture(autouse=True)
def patch_store_dirs(tmp_path, monkeypatch):
    """Override store dirs to use tmp_path."""
    singleton_dir = tmp_path / ".singleton"
    threads_dir = singleton_dir / "threads"
    monkeypatch.setattr(store, "SINGLETON_DIR", singleton_dir)
    monkeypatch.setattr(store, "THREADS_DIR", threads_dir)
    store.init_dirs()
    return singleton_dir


def make_daemon(tmp_path: Path) -> Daemon:
    """Create a daemon with tmp_path as state dir."""
    d = Daemon(mcp_port=33100, state_dir=tmp_path / ".singleton")
    return d


# T-DAEMON-8: daemon.pid written on start, removed on stop
async def test_daemon_pid_written_and_removed(tmp_path):
    import tempfile

    # Use /tmp to keep socket path short (macOS AF_UNIX limit is 104 chars)
    with tempfile.TemporaryDirectory(dir="/tmp") as short_dir:
        state_dir = Path(short_dir) / ".s"
        state_dir.mkdir()
        store.init_dirs.__module__  # access to ensure import
        d = Daemon(mcp_port=33100, state_dir=state_dir)

        mock_hub = MagicMock()
        mock_hub.start = AsyncMock()
        mock_hub.stop = AsyncMock()
        mock_hub.read_loop = AsyncMock()
        mock_hub.resize = MagicMock()
        mock_hub.write = MagicMock()

        with (
            patch("singleton.daemon.Hub", return_value=mock_hub),
            patch.object(d, "_run_mcp_server", new=AsyncMock()),
            patch.object(d, "_wait_for_mcp_ready", new=AsyncMock()),
            patch.object(d, "_crash_recovery", new=AsyncMock()),
            patch.object(d, "_file_watcher_loop", new=AsyncMock()),
            patch.object(d, "_hub_read_loop", new=AsyncMock()),
        ):
            await d.start()

            pid_path = d.state_dir / "daemon.pid"
            assert pid_path.exists()
            assert int(pid_path.read_text().strip()) == os.getpid()

            await d.stop()

            assert not pid_path.exists()


# T-DAEMON-9: Unix socket created on start, removed on stop
async def test_daemon_socket_created_and_removed(tmp_path):
    import tempfile

    with tempfile.TemporaryDirectory(dir="/tmp") as short_dir:
        state_dir = Path(short_dir) / ".s"
        state_dir.mkdir()
        d = Daemon(mcp_port=33101, state_dir=state_dir)

        mock_hub = MagicMock()
        mock_hub.start = AsyncMock()
        mock_hub.stop = AsyncMock()
        mock_hub.read_loop = AsyncMock()
        mock_hub.resize = MagicMock()
        mock_hub.write = MagicMock()

        with (
            patch("singleton.daemon.Hub", return_value=mock_hub),
            patch.object(d, "_run_mcp_server", new=AsyncMock()),
            patch.object(d, "_wait_for_mcp_ready", new=AsyncMock()),
            patch.object(d, "_crash_recovery", new=AsyncMock()),
            patch.object(d, "_file_watcher_loop", new=AsyncMock()),
            patch.object(d, "_hub_read_loop", new=AsyncMock()),
        ):
            await d.start()

            sock_path = d.state_dir / "daemon.sock"
            assert sock_path.exists()

            await d.stop()

            assert not sock_path.exists()


# T-DAEMON-2: Injection queued when hub_busy=True
async def test_injection_queued_when_busy(tmp_path):
    d = make_daemon(tmp_path)
    d.hub_busy = True
    d._running = True

    await d._enqueue_injection("test message")

    assert d.injection_queue.qsize() == 1


# T-DAEMON-4: Queue max 10; 11th dropped
async def test_injection_queue_max(tmp_path):
    d = make_daemon(tmp_path)
    d.hub_busy = True
    d._running = True

    for i in range(10):
        await d._enqueue_injection(f"message {i}")

    assert d.injection_queue.qsize() == 10

    # 11th should be dropped (queue full)
    await d._enqueue_injection("overflow message")
    assert d.injection_queue.qsize() == 10  # Still 10


# T-DAEMON-3: Queued injection fires after hub quiet >200ms
async def test_injection_fires_after_quiet(tmp_path):
    d = make_daemon(tmp_path)
    d._running = True
    injected: list[str] = []

    async def mock_inject(text: str):
        injected.append(text)

    d._inject = mock_inject  # type: ignore

    # Put something in the queue
    await d._enqueue_injection("hello")

    # Mark as busy then let it settle
    d.hub_busy = True
    await asyncio.sleep(0.05)
    d.hub_busy = False

    # Start injection loop briefly
    task = asyncio.create_task(d._injection_loop())
    await asyncio.sleep(0.3)
    d._running = False
    task.cancel()
    try:
        await task
    except (asyncio.CancelledError, Exception):
        pass

    assert len(injected) >= 1
    assert "hello" in injected[0]


# T-DAEMON-1: File watcher detects new event within 500ms
async def test_file_watcher_detects_events(tmp_path):
    d = make_daemon(tmp_path)
    d._running = True

    detected_events: list[Path] = []

    async def mock_handle_event(event_path: Path):
        detected_events.append(event_path)

    d._handle_event = mock_handle_event  # type: ignore

    # Create a thread and write an event
    thread = store.create_thread(description="Watcher test")
    tid = thread["id"]

    # Start file watcher
    task = asyncio.create_task(d._file_watcher_loop())
    await asyncio.sleep(0.2)  # Let watcher start

    # Write an event file
    store.write_event(tid, "stop", {"session_id": "s1"})

    # Wait for detection
    for _ in range(20):
        if detected_events:
            break
        await asyncio.sleep(0.05)

    d._running = False
    task.cancel()
    try:
        await task
    except (asyncio.CancelledError, Exception):
        pass

    assert len(detected_events) >= 1
    assert detected_events[0].suffix == ".json"
    assert "events" in str(detected_events[0])


# T-DAEMON-11: Multi-attach fan-out
async def test_fan_out_to_multiple_clients(tmp_path):
    d = make_daemon(tmp_path)

    received: list[list[bytes]] = [[], []]

    class MockWriter:
        def __init__(self, idx: int):
            self.idx = idx

        def write(self, data: bytes):
            received[self.idx].append(data)

    w1 = MockWriter(0)
    w2 = MockWriter(1)
    d.cli_connections = [w1, w2]  # type: ignore

    data = b"hello world"
    d._fan_out_hub_output(data)

    assert len(received[0]) == 1
    assert len(received[1]) == 1

    # Decode and check
    msg1 = json.loads(received[0][0].decode().strip())
    assert msg1["type"] == "output"
    assert base64.b64decode(msg1["data"]) == data


# T-DAEMON-12: Input forwarding from CLI to hub
async def test_input_forwarding(tmp_path):
    d = make_daemon(tmp_path)
    d._running = True

    written: list[bytes] = []

    mock_hub = MagicMock()
    mock_hub.write = lambda data: written.append(data)
    mock_hub.resize = MagicMock()
    d.hub = mock_hub

    # Simulate CLI connection sending input
    input_data = b"hello claude"
    encoded = base64.b64encode(input_data).decode()
    msg_line = json.dumps({"type": "input", "data": encoded}) + "\n"
    msg_bytes = msg_line.encode()

    # Create a pipe to simulate the connection
    reader_fd, writer_fd = os.pipe()
    os.write(writer_fd, msg_bytes)
    os.close(writer_fd)

    reader = asyncio.StreamReader()
    protocol = asyncio.StreamReaderProtocol(reader)
    loop = asyncio.get_event_loop()
    transport, _ = await loop.connect_read_pipe(lambda: protocol, os.fdopen(reader_fd))

    mock_writer = MagicMock()
    mock_writer.write = MagicMock()
    mock_writer.wait_closed = AsyncMock()

    # Write EOF to trigger connection close after one message
    # Run handler briefly
    task = asyncio.create_task(d._handle_cli_connection(reader, mock_writer))
    await asyncio.sleep(0.2)
    task.cancel()
    try:
        await task
    except (asyncio.CancelledError, Exception):
        pass

    transport.close()

    assert len(written) >= 1
    assert written[0] == input_data


# T-DAEMON-5: stop event updates thread status
async def test_handle_stop_event(tmp_path):
    d = make_daemon(tmp_path)
    d._running = True
    d.hub = MagicMock()
    d.hub.write = MagicMock()
    d.injection_queue = asyncio.Queue(maxsize=10)

    thread = store.create_thread(description="Stop event test")
    tid = thread["id"]
    store.update_thread_status(tid, "running")

    event = store.write_event(tid, "stop", {"session_id": "s1"})
    event_path = store.get_thread_dir(tid) / "events" / f"{event['event_id']}.json"

    await d._handle_event(event_path)

    t = store.get_thread(tid)
    assert t["status"] == "idle"


# T-DAEMON-6: pretool supervised event injects approval request
async def test_handle_pretool_supervised(tmp_path):
    d = make_daemon(tmp_path)
    d._running = True
    d.hub = MagicMock()
    d.hub.write = MagicMock()
    d.injection_queue = asyncio.Queue(maxsize=10)

    thread = store.create_thread(
        description="Pretool test", permissions_mode="supervised"
    )
    tid = thread["id"]

    req = store.write_pending(tid, "Bash", {"command": "ls"}, "supervised")
    event = store.write_event(
        tid,
        "pretool",
        {
            "request_id": req["request_id"],
            "tool": "Bash",
            "input": {"command": "ls"},
            "mode": "supervised",
        },
    )
    event_path = store.get_thread_dir(tid) / "events" / f"{event['event_id']}.json"

    await d._handle_event(event_path)

    t = store.get_thread(tid)
    assert t["status"] == "awaiting_approval"
    assert d.injection_queue.qsize() == 1


# T-DAEMON-7: crash recovery marks disconnected threads
async def test_crash_recovery(tmp_path):
    d = make_daemon(tmp_path)

    thread = store.create_thread(description="Crash test")
    tid = thread["id"]
    # Set to running with a non-existent PID
    store.update_thread(tid, status="running", pid=99999999)

    await d._crash_recovery()

    t = store.get_thread(tid)
    # PID doesn't exist → done
    assert t["status"] == "done"


# T-DAEMON-10: WorkerManager delegates correctly
async def test_worker_manager_delegates(tmp_path):
    d = make_daemon(tmp_path)
    spawn_mock: AsyncMock = AsyncMock()
    send_mock: AsyncMock = AsyncMock(return_value="result")
    cancel_mock: AsyncMock = AsyncMock()
    d.spawn_thread_worker = spawn_mock  # type: ignore[assignment]
    d.send_to_worker = send_mock  # type: ignore[assignment]
    d.cancel_worker = cancel_mock  # type: ignore[assignment]

    mgr = WorkerManager(d)

    await mgr.spawn("tid1")
    spawn_mock.assert_called_once_with("tid1")

    result = await mgr.send("tid1", "hello")
    assert result == "result"
    send_mock.assert_called_once_with("tid1", "hello")

    await mgr.cancel("tid1")
    cancel_mock.assert_called_once_with("tid1")
