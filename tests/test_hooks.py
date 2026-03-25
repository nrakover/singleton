"""Tests for direct Python hook entrypoints."""

from __future__ import annotations

import json
import os
import subprocess
import sys
import threading
import time

import singleton.store as store


def test_session_start_hook_writes_run_started(tmp_path, monkeypatch):
    _configure_store(tmp_path, monkeypatch)
    thread = store.create_thread(description="Thread")
    run = store.create_run(thread["thread_id"])

    result = _run_hook(
        "session-start",
        thread["thread_id"],
        run["run_id"],
        {"session_id": "sess-123", "source": "startup"},
    )

    assert result.returncode == 0
    events = store.get_thread_events(thread["thread_id"])
    assert events["events"][0]["message_type"] == "run_started"
    assert events["events"][0]["payload"]["session_id"] == "sess-123"


def test_permission_request_hook_writes_request(tmp_path, monkeypatch):
    _configure_store(tmp_path, monkeypatch)
    thread = store.create_thread(description="Thread")
    run = store.create_run(thread["thread_id"])

    def approve() -> None:
        _wait_for_message_count(thread["thread_id"], 1)
        store.append_message(
            direction="to_worker",
            message_type="permission_resolution",
            thread_id=thread["thread_id"],
            run_id=run["run_id"],
            payload={"request_id": "req-1", "decision": "allow", "resolved_by": "hub"},
        )

    worker = threading.Thread(target=approve)
    worker.start()
    result = _run_hook(
        "permission-request",
        thread["thread_id"],
        run["run_id"],
        {
            "request_id": "req-1",
            "tool_name": "Bash",
            "tool_input": {"command": "git push"},
            "permission_suggestions": [],
        },
    )
    worker.join()

    assert result.returncode == 0
    payload = json.loads(result.stdout.decode())
    assert payload["hookSpecificOutput"]["decision"]["behavior"] == "allow"
    pending = store.list_pending_approvals()
    assert pending == []


def test_permission_request_hook_denies_with_reason(tmp_path, monkeypatch):
    _configure_store(tmp_path, monkeypatch)
    thread = store.create_thread(description="Thread")
    run = store.create_run(thread["thread_id"])

    def deny() -> None:
        _wait_for_message_count(thread["thread_id"], 1)
        store.append_message(
            direction="to_worker",
            message_type="permission_resolution",
            thread_id=thread["thread_id"],
            run_id=run["run_id"],
            payload={
                "request_id": "req-2",
                "decision": "deny",
                "resolved_by": "hub",
                "reason": "Too risky",
            },
        )

    worker = threading.Thread(target=deny)
    worker.start()
    result = _run_hook(
        "permission-request",
        thread["thread_id"],
        run["run_id"],
        {
            "request_id": "req-2",
            "tool_name": "Bash",
            "tool_input": {"command": "rm -rf /"},
        },
    )
    worker.join()

    assert result.returncode == 0
    payload = json.loads(result.stdout.decode())
    assert payload["hookSpecificOutput"]["decision"]["behavior"] == "deny"
    assert payload["hookSpecificOutput"]["decision"]["message"] == "Too risky"


def test_permission_request_hook_times_out(tmp_path, monkeypatch):
    _configure_store(tmp_path, monkeypatch)
    thread = store.create_thread(description="Thread")
    run = store.create_run(thread["thread_id"])

    result = _run_hook(
        "permission-request",
        thread["thread_id"],
        run["run_id"],
        {
            "request_id": "req-timeout",
            "tool_name": "Edit",
            "tool_input": {"file_path": "x.py"},
        },
        extra_env={
            "SINGLETON_PERMISSION_TIMEOUT": "0.2",
            "SINGLETON_PERMISSION_POLL_INTERVAL": "0.05",
        },
    )

    assert result.returncode == 2


def test_stop_hook_writes_completed_run_finished(tmp_path, monkeypatch):
    _configure_store(tmp_path, monkeypatch)
    thread = store.create_thread(description="Thread")
    run = store.create_run(thread["thread_id"])

    result = _run_hook(
        "stop",
        thread["thread_id"],
        run["run_id"],
        {"session_id": "sess-123", "last_assistant_message": "Done."},
    )

    assert result.returncode == 0
    event = store.get_thread_events(thread["thread_id"])["events"][0]
    assert event["message_type"] == "run_finished"
    assert event["payload"]["outcome"] == "completed"
    assert event["payload"]["result_text"] == "Done."


def test_stop_failure_hook_writes_api_error(tmp_path, monkeypatch):
    _configure_store(tmp_path, monkeypatch)
    thread = store.create_thread(description="Thread")
    run = store.create_run(thread["thread_id"])

    result = _run_hook(
        "stop-failure",
        thread["thread_id"],
        run["run_id"],
        {
            "session_id": "sess-123",
            "error": "rate_limit",
            "error_details": "429 Too Many Requests",
            "last_assistant_message": "API Error",
        },
    )

    assert result.returncode == 0
    event = store.get_thread_events(thread["thread_id"])["events"][0]
    assert event["payload"]["outcome"] == "api_error"
    assert event["payload"]["error"] == "rate_limit"


def test_notification_hook_writes_notification_message(tmp_path, monkeypatch):
    _configure_store(tmp_path, monkeypatch)
    thread = store.create_thread(description="Thread")
    run = store.create_run(thread["thread_id"])

    result = _run_hook(
        "notification",
        thread["thread_id"],
        run["run_id"],
        {"message": "Heads up"},
    )

    assert result.returncode == 0
    event = store.get_thread_events(thread["thread_id"])["events"][0]
    assert event["message_type"] == "notification"
    assert event["payload"]["text"] == "Heads up"


def _run_hook(
    hook_name: str,
    thread_id: str,
    run_id: str,
    stdin_data: dict,
    extra_env: dict[str, str] | None = None,
) -> subprocess.CompletedProcess[bytes]:
    env = os.environ.copy()
    env.update(
        {
            "SINGLETON_THREAD_ID": thread_id,
            "SINGLETON_RUN_ID": run_id,
            "SINGLETON_STATE_DIR": str(store.SINGLETON_DIR),
            "SINGLETON_DB_PATH": str(store.DB_PATH),
        }
    )
    if extra_env:
        env.update(extra_env)
    return subprocess.run(
        [sys.executable, "-m", "singleton.hooks", hook_name],
        input=json.dumps(stdin_data).encode(),
        capture_output=True,
        env=env,
        timeout=10,
    )


def _wait_for_message_count(thread_id: str, count: int) -> None:
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        if store.get_thread_events(thread_id)["total"] >= count:
            return
        time.sleep(0.05)
    raise AssertionError("timed out waiting for messages")


def _configure_store(tmp_path, monkeypatch):
    singleton_dir = tmp_path / ".singleton"
    monkeypatch.setattr(store, "SINGLETON_DIR", singleton_dir)
    monkeypatch.setattr(store, "THREADS_DIR", singleton_dir / "threads")
    monkeypatch.setattr(store, "DB_PATH", singleton_dir / "messages.db")
    store.init_dirs()
