"""Tests for hook scripts (T-HOOKS-1 through T-HOOKS-7)."""

import json
import subprocess
import time
from pathlib import Path

import pytest

import singleton.store as store

HOOKS_DIR = Path(__file__).parent.parent / "hooks"


@pytest.fixture(autouse=True)
def patch_store_dirs(tmp_path, monkeypatch):
    """Override store dirs to use tmp_path."""
    singleton_dir = tmp_path / ".singleton"
    threads_dir = singleton_dir / "threads"
    monkeypatch.setattr(store, "SINGLETON_DIR", singleton_dir)
    monkeypatch.setattr(store, "THREADS_DIR", threads_dir)
    store.init_dirs()
    return singleton_dir


def _run_hook(
    script: str,
    thread_id: str,
    state_dir: Path,
    stdin_data: dict | None = None,
    extra_env: dict | None = None,
    timeout: int = 10,
) -> subprocess.CompletedProcess:
    """Run a hook script with proper env vars."""
    import os

    env = os.environ.copy()
    env["SINGLETON_THREAD_ID"] = thread_id
    env["SINGLETON_STATE_DIR"] = str(state_dir)
    env["SINGLETON_HOOKS_DIR"] = str(HOOKS_DIR)
    if extra_env:
        env.update(extra_env)

    stdin_bytes = json.dumps(stdin_data).encode() if stdin_data is not None else b"{}"

    return subprocess.run(
        [str(HOOKS_DIR / script)],
        input=stdin_bytes,
        capture_output=True,
        env=env,
        timeout=timeout,
    )


# T-HOOKS-1: worker-stop.sh creates event file with stop type
def test_stop_hook_creates_event(tmp_path):
    thread = store.create_thread(description="Stop hook test")
    tid = thread["id"]
    state_dir = store.SINGLETON_DIR

    result = _run_hook(
        "worker-stop.sh",
        tid,
        state_dir,
        stdin_data={"session_id": "sess-abc123"},
    )

    assert result.returncode == 0, result.stderr.decode()

    events_dir = store.get_thread_dir(tid) / "events"
    event_files = list(events_dir.glob("*.json"))
    assert len(event_files) == 1

    event = json.loads(event_files[0].read_text())
    assert event["type"] == "stop"
    assert event["thread_id"] == tid
    assert event["data"]["session_id"] == "sess-abc123"
    assert "event_id" in event
    assert "timestamp" in event


# T-HOOKS-2: worker-stop.sh event_id format
def test_stop_hook_event_id_format(tmp_path):
    thread = store.create_thread(description="Event ID test")
    tid = thread["id"]
    state_dir = store.SINGLETON_DIR

    result = _run_hook("worker-stop.sh", tid, state_dir, stdin_data={"session_id": ""})
    assert result.returncode == 0

    events_dir = store.get_thread_dir(tid) / "events"
    event_files = list(events_dir.glob("*.json"))
    event = json.loads(event_files[0].read_text())

    # event_id format: {unix_ms}-stop-{random4}
    parts = event["event_id"].split("-")
    assert len(parts) == 3
    assert int(parts[0]) > 0  # unix ms
    assert parts[1] == "stop"
    assert len(parts[2]) == 4  # 2 hex bytes = 4 chars


# T-HOOKS-3: worker-notify.sh creates notification event
def test_notify_hook_creates_event(tmp_path):
    thread = store.create_thread(description="Notify hook test")
    tid = thread["id"]
    state_dir = store.SINGLETON_DIR

    result = _run_hook(
        "worker-notify.sh",
        tid,
        state_dir,
        stdin_data={"message": "Task completed"},
    )

    assert result.returncode == 0, result.stderr.decode()

    events_dir = store.get_thread_dir(tid) / "events"
    event_files = list(events_dir.glob("*.json"))
    assert len(event_files) == 1

    event = json.loads(event_files[0].read_text())
    assert event["type"] == "notification"
    assert event["thread_id"] == tid
    assert event["data"]["message"] == "Task completed"


# T-HOOKS-4: worker-pretool.sh yolo mode exits 0 without creating pending
def test_pretool_hook_yolo_exits_0(tmp_path):
    thread = store.create_thread(
        description="Yolo pretool test", permissions_mode="yolo"
    )
    tid = thread["id"]
    state_dir = store.SINGLETON_DIR

    result = _run_hook(
        "worker-pretool.sh",
        tid,
        state_dir,
        stdin_data={"tool_name": "Bash", "tool_input": {"command": "ls"}},
    )

    assert result.returncode == 0
    # No pending files created
    pending_dir = store.get_thread_dir(tid) / "pending"
    assert len(list(pending_dir.glob("*.json"))) == 0


# T-HOOKS-5: worker-pretool.sh supervised mode creates pending, exits 2 on timeout
def test_pretool_hook_supervised_timeout(tmp_path):
    thread = store.create_thread(
        description="Supervised pretool test", permissions_mode="supervised"
    )
    tid = thread["id"]
    state_dir = store.SINGLETON_DIR

    # Use short timeout (3 seconds)
    result = _run_hook(
        "worker-pretool.sh",
        tid,
        state_dir,
        stdin_data={"tool_name": "Bash", "tool_input": {"command": "ls"}},
        extra_env={"SINGLETON_PRETOOL_TIMEOUT": "3"},
        timeout=15,
    )

    assert result.returncode == 2

    # Pending file should exist
    pending_dir = store.get_thread_dir(tid) / "pending"
    pending_files = list(pending_dir.glob("*.json"))
    assert len(pending_files) == 1

    pending = json.loads(pending_files[0].read_text())
    assert pending["tool"] == "Bash"
    assert pending["mode"] == "supervised"


# T-HOOKS-6: worker-pretool.sh supervised mode approves when response written
def test_pretool_hook_supervised_approve(tmp_path):
    thread = store.create_thread(
        description="Approved pretool test", permissions_mode="supervised"
    )
    tid = thread["id"]
    state_dir = store.SINGLETON_DIR

    # We need to write the response file asynchronously while the hook polls
    # Use a thread to write the response after a short delay
    import threading

    response_written = threading.Event()

    def write_approval():
        # Wait a bit for the pending file to be created
        time.sleep(1.5)
        pending_dir = store.get_thread_dir(tid) / "pending"
        # Wait for pending file
        for _ in range(20):
            files = list(pending_dir.glob("*.json"))
            if files:
                break
            time.sleep(0.1)

        if files:
            req = json.loads(files[0].read_text())
            req_id = req["request_id"]
            store.write_response(req_id, tid, "approve", "hub")
        response_written.set()

    t = threading.Thread(target=write_approval)
    t.start()

    result = _run_hook(
        "worker-pretool.sh",
        tid,
        state_dir,
        stdin_data={"tool_name": "Write", "tool_input": {"path": "/tmp/x"}},
        extra_env={"SINGLETON_PRETOOL_TIMEOUT": "30"},
        timeout=15,
    )

    t.join(timeout=5)
    assert result.returncode == 0


# T-HOOKS-7: worker-pretool.sh supervised mode denies when response is deny
def test_pretool_hook_supervised_deny(tmp_path):
    thread = store.create_thread(
        description="Denied pretool test", permissions_mode="supervised"
    )
    tid = thread["id"]
    state_dir = store.SINGLETON_DIR

    import threading

    def write_denial():
        time.sleep(1.5)
        pending_dir = store.get_thread_dir(tid) / "pending"
        for _ in range(20):
            files = list(pending_dir.glob("*.json"))
            if files:
                break
            time.sleep(0.1)

        if files:
            req = json.loads(files[0].read_text())
            req_id = req["request_id"]
            store.write_response(req_id, tid, "deny", "hub")

    t = threading.Thread(target=write_denial)
    t.start()

    result = _run_hook(
        "worker-pretool.sh",
        tid,
        state_dir,
        stdin_data={"tool_name": "Bash", "tool_input": {"command": "rm -rf /"}},
        extra_env={"SINGLETON_PRETOOL_TIMEOUT": "30"},
        timeout=15,
    )

    t.join(timeout=5)
    assert result.returncode == 2
    assert b"denied" in result.stdout.lower() or b"denied" in result.stderr.lower()
