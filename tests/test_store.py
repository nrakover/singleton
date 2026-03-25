"""Tests for the SQLite-backed singleton store."""

from __future__ import annotations

import json
import threading

import singleton.store as store


def test_schema_bootstrap_creates_tables(tmp_path, monkeypatch):
    singleton_dir = _configure_store(tmp_path, monkeypatch)

    with store.connect() as conn:
        table_names = {
            row[0]
            for row in conn.execute(
                "SELECT name FROM sqlite_master WHERE type = 'table'"
            ).fetchall()
        }

    assert {"threads", "runs", "messages"}.issubset(table_names)
    assert (singleton_dir / "messages.db").exists()


def test_create_thread_persists_default_worker_cwd(tmp_path, monkeypatch):
    _configure_store(tmp_path, monkeypatch)

    thread = store.create_thread(description="Test thread")

    assert thread["cwd"] == str(store.SINGLETON_DIR / "workers" / "default")
    assert thread["session_id"] is None


def test_create_run_persists_row_before_launch(tmp_path, monkeypatch):
    _configure_store(tmp_path, monkeypatch)
    thread = store.create_thread(description="Run parent")

    run = store.create_run(thread["thread_id"])

    assert run["thread_id"] == thread["thread_id"]
    assert run["pid"] is None
    assert run["finished_at"] is None

    with store.connect() as conn:
        row = conn.execute(
            "SELECT run_id, thread_id FROM runs WHERE run_id = ?", (run["run_id"],)
        ).fetchone()

    assert tuple(row) == (run["run_id"], thread["thread_id"])


def test_append_message_persists_expected_fields(tmp_path, monkeypatch):
    _configure_store(tmp_path, monkeypatch)
    thread = store.create_thread(description="Thread")
    run = store.create_run(thread["thread_id"])

    message = store.append_message(
        direction="from_worker",
        message_type="run_started",
        thread_id=thread["thread_id"],
        run_id=run["run_id"],
        payload={"session_id": "sess-123", "source": "startup"},
    )

    assert message["direction"] == "from_worker"
    assert message["message_type"] == "run_started"
    assert message["payload"]["session_id"] == "sess-123"


def test_update_thread_session_is_sparse(tmp_path, monkeypatch):
    _configure_store(tmp_path, monkeypatch)
    thread = store.create_thread(description="Thread", permissions_mode="supervised")

    updated = store.update_thread(thread["thread_id"], session_id="sess-123")

    assert updated["session_id"] == "sess-123"
    assert updated["description"] == thread["description"]
    assert updated["permissions_mode"] == thread["permissions_mode"]


def test_update_run_sparse_fields(tmp_path, monkeypatch):
    _configure_store(tmp_path, monkeypatch)
    thread = store.create_thread(description="Thread")
    run = store.create_run(thread["thread_id"])

    updated = store.update_run(run["run_id"], pid=123, exit_code=0)
    finished = store.update_run(run["run_id"], finished_at="2026-03-22T12:00:00.000Z")

    assert updated["pid"] == 123
    assert updated["exit_code"] == 0
    assert finished["finished_at"] == "2026-03-22T12:00:00.000Z"


def test_list_pending_approvals_derives_from_messages(tmp_path, monkeypatch):
    _configure_store(tmp_path, monkeypatch)
    thread = store.create_thread(description="Thread")
    run = store.create_run(thread["thread_id"])

    req1 = store.append_message(
        direction="from_worker",
        message_type="permission_request",
        thread_id=thread["thread_id"],
        run_id=run["run_id"],
        payload={
            "request_id": "req-1",
            "tool_name": "Bash",
            "tool_input": {"command": "git push"},
            "permission_mode": "supervised",
        },
    )
    store.append_message(
        direction="from_worker",
        message_type="permission_request",
        thread_id=thread["thread_id"],
        run_id=run["run_id"],
        payload={
            "request_id": "req-2",
            "tool_name": "Edit",
            "tool_input": {"file_path": "x.py"},
            "permission_mode": "supervised",
        },
    )
    store.append_message(
        direction="to_worker",
        message_type="permission_resolution",
        thread_id=thread["thread_id"],
        run_id=run["run_id"],
        payload={"request_id": "req-2", "decision": "approve", "resolved_by": "hub"},
    )

    pending = store.list_pending_approvals()

    assert len(pending) == 1
    assert pending[0]["request_id"] == req1["payload"]["request_id"]


def test_get_thread_events_returns_newest_first(tmp_path, monkeypatch):
    _configure_store(tmp_path, monkeypatch)
    thread = store.create_thread(description="Thread")
    run = store.create_run(thread["thread_id"])

    first = store.append_message(
        direction="from_worker",
        message_type="notification",
        thread_id=thread["thread_id"],
        run_id=run["run_id"],
        payload={"text": "first"},
    )
    second = store.append_message(
        direction="from_worker",
        message_type="run_finished",
        thread_id=thread["thread_id"],
        run_id=run["run_id"],
        payload={"outcome": "completed", "session_id": "sess", "result_text": "done"},
    )

    page = store.get_thread_events(thread["thread_id"], page=0, page_size=10)

    assert page["events"][0]["message_id"] == second["message_id"]
    assert page["events"][1]["message_id"] == first["message_id"]


def test_concurrent_append_message_writes_are_safe(tmp_path, monkeypatch):
    _configure_store(tmp_path, monkeypatch)
    thread = store.create_thread(description="Thread")
    run = store.create_run(thread["thread_id"])

    def worker(index: int) -> None:
        for item in range(20):
            store.append_message(
                direction="from_worker",
                message_type="notification",
                thread_id=thread["thread_id"],
                run_id=run["run_id"],
                payload={"text": f"{index}-{item}"},
            )

    threads = [threading.Thread(target=worker, args=(index,)) for index in range(5)]
    for thread_obj in threads:
        thread_obj.start()
    for thread_obj in threads:
        thread_obj.join()

    page = store.get_thread_events(thread["thread_id"], page=0, page_size=200)

    assert page["total"] == 100


def _configure_store(tmp_path, monkeypatch):
    singleton_dir = tmp_path / ".singleton"
    monkeypatch.setattr(store, "SINGLETON_DIR", singleton_dir)
    monkeypatch.setattr(store, "THREADS_DIR", singleton_dir / "threads")
    monkeypatch.setattr(store, "DB_PATH", singleton_dir / "messages.db")
    store.init_dirs()
    return singleton_dir
